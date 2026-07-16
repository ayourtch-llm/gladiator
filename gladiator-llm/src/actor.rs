use crate::config::merge_config;
use crate::event::{LlmEvent, StreamStats};
use crate::generate_object::GenerateObject;
use crate::protocol::{Protocol, StreamState};
use crate::provider::ProviderConfig;
use crate::request::{CanonicalRequest, LlmRequest};
use crate::tool_runtime::ToolRuntime;
use gladiator_core::Actor;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// Outcome of a streaming response, beyond the raw text/tool_calls. The agent
/// uses this to decide whether to triage (idle / stuck-loop) or proceed normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSignal {
    /// Stream completed normally (or was interrupted by an error handled elsewhere).
    Normal,
    /// No tokens arrived for the idle threshold (90s). The model went silent.
    Idle,
    /// The model was actively emitting tokens but repeating itself (think-loop).
    /// Detected by `LoopDetector` comparing consecutive text/reasoning windows.
    StuckLoop,
}

impl Default for StreamSignal {
    fn default() -> Self {
        StreamSignal::Normal
    }
}

#[derive(Debug, Default)]
pub struct LlmActor {
    pub index: usize,
    pub input_topic: String,
    pub output_topic: String,
    pub stream_topic: String,
    pub stats_topic: String,
    pub tool_calls_topic: String,
    pub control_topic: String,
    pub config: gladiator_core::LlmConfig,
    tool_runtime: Arc<Mutex<ToolRuntime>>,
}

impl LlmActor {
    pub fn new(
        index: usize,
        input_topic: String,
        output_topic: String,
        stream_topic: String,
        stats_topic: String,
        tool_calls_topic: String,
        control_topic: String,
        config: gladiator_core::LlmConfig,
    ) -> Self {
        Self {
            index,
            input_topic,
            output_topic,
            stream_topic,
            stats_topic,
            tool_calls_topic,
            control_topic,
            config,
            tool_runtime: Arc::new(Mutex::new(ToolRuntime::new())),
        }
    }

    pub fn build_request_body(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        tools: Option<&[serde_json::Value]>,
        grammar: Option<&str>,
    ) -> serde_json::Value {
        let canonical = CanonicalRequest {
            model: config.model.clone(),
            messages: messages.to_vec(),
            tools: tools.map(|t| t.to_vec()),
            tool_choice: None,
            temperature: Some(config.temperature),
            max_tokens: Some(config.max_tokens),
            stream: true,
            grammar: grammar.map(|s| s.to_string()),
        };

        let provider = ProviderConfig::new("openai", &config.base_url, &config.api_key);
        let route = provider.openai_chat_route();
        route.build_body(&canonical)
    }

    async fn publish_stream_end_and_stats(
        &self,
        bus: &gladiator_core::Bus,
        stream_id: &str,
        rx_chars: usize,
        usage: Option<crate::event::Usage>,
        context_window: Option<usize>,
    ) {
        let end_msg = gladiator_core::Message::new(
            &self.stream_topic,
            &self.id(),
            serde_json::json!({"type": "stream_end"}),
        )
        .with_type("LlmStreamEnd")
        .with_stream_id(stream_id.to_string());
        if let Err(e) = bus.publish(&self.id(), end_msg).await {
            tracing::error!("Failed to publish stream end: {}", e);
        }

        let stats = StreamStats {
            rx_chars,
            usage,
            context_window,
        };
        let stats_msg = gladiator_core::Message::new(
            &self.stats_topic,
            &self.id(),
            serde_json::to_value(&stats).unwrap_or_else(|e| {
                tracing::error!("Failed to serialize StreamStats: {}", e);
                serde_json::to_value(&StreamStats {
                    rx_chars,
                    usage: None,
                    context_window: None,
                })
                .unwrap()
            }),
        )
        .with_type("StreamStats")
        .with_stream_id(stream_id.to_string());
        if let Err(e) = bus.publish(&self.id(), stats_msg).await {
            tracing::error!("Failed to publish stream stats: {}", e);
        }
    }

    async fn stream_response(
        &self,
        response: reqwest::Response,
        config: &gladiator_core::LlmConfig,
        bus: &gladiator_core::Bus,
        stream_id: &str,
        protocol: &dyn Protocol,
        offered_tools: Option<&[serde_json::Value]>,
        _tool_runtime: &Arc<Mutex<ToolRuntime>>,
    ) -> Result<(String, Vec<serde_json::Value>, StreamSignal), crate::error::LlmError> {
        let mut full_response = String::new();
        let mut reasoning_response = String::new();
        let mut stream = response.bytes_stream();
        let mut rx_chars: usize = 0;
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut state = StreamState::default();
        let stream_timeout = std::time::Duration::from_secs(config.stream_timeout_secs);
        const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
        let mut last_activity = std::time::Instant::now();
        let mut signal = StreamSignal::Normal;
        // Loop detector: catches active streams where the model repeats itself
        // (think-loops, dead-end snippets) — a case the idle timeout misses.
        let mut loop_detector = crate::similarity::LoopDetector::new();
        if config.loop_cycle_window > 0 {
            loop_detector.cycle_window = config.loop_cycle_window;
        }
        if config.loop_max_total_chars > 0 {
            loop_detector.max_total_chars = config.loop_max_total_chars;
        }

        loop {
            let per_chunk = std::cmp::min(
                stream_timeout,
                IDLE_TIMEOUT
                    .saturating_sub(last_activity.elapsed())
                    .max(std::time::Duration::from_millis(1)),
            );
            match tokio::time::timeout(per_chunk, stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    last_activity = std::time::Instant::now();
                    debug!("[llm] received chunk of {} bytes", chunk.len());
                    let payloads = crate::framing::decode_sse_chunk(&chunk);
                    for payload in payloads {
                        debug!("[llm] SSE payload: {}", &payload[..payload.len().min(200)]);
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&payload) {
                            let events = protocol.parse_event(&json, &mut state);
                            debug!("[llm] parsed {} events", events.len());

                            for event in events {
                                match event {
                                    LlmEvent::TextDelta { text, .. } => {
                                        if !text.is_empty() {
                                            let preview: String = text.chars().take(80).collect();
                                            info!("[llm{}] text delta: len={}, preview={}", self.index, text.len(), preview);
                                            rx_chars += text.chars().count();
                                            let chunk_msg = gladiator_core::Message::new(
                                                &self.stream_topic,
                                                &self.id(),
                                                text.clone(),
                                            )
                                            .with_type("LlmStream")
                                            .with_stream_id(stream_id.to_string());
                                            let _ = bus.publish(&self.id(), chunk_msg).await;
                                            full_response.push_str(&text);
                                            if loop_detector.push(&text).is_some() {
                                                signal = StreamSignal::StuckLoop;
                                                break;
                                            }
                                        }
                                    }
                                    LlmEvent::ReasoningDelta { text, .. } => {
                                        if !text.is_empty() {
                                            let preview: String = text.chars().take(80).collect();
                                            info!("[llm{}] reasoning delta: len={}, preview={}", self.index, text.len(), preview);
                                            rx_chars += text.chars().count();
                                            let chunk_msg = gladiator_core::Message::new(
                                                &self.stream_topic,
                                                &self.id(),
                                                text.clone(),
                                            )
                                            .with_type("LlmThinking")
                                            .with_stream_id(stream_id.to_string());
                                            let _ = bus.publish(&self.id(), chunk_msg).await;
                                            reasoning_response.push_str(&text);
                                            // Reasoning loops are the most common failure
                                            // mode — feed them into the same detector.
                                            if loop_detector.push(&text).is_some() {
                                                signal = StreamSignal::StuckLoop;
                                                break;
                                            }
                                        }
                                    }
                                    LlmEvent::ToolInputStart { index, ref name, ref id, .. } => {
                                        // Publish tool call start to TUI for progress display.
                                        // Include `index` and `id` so the TUI can match
                                        // multiple concurrent tool calls by stable key.
                                        let tc_payload = serde_json::json!({
                                            "index": index,
                                            "id": id,
                                            "function": {
                                                "name": name,
                                                "arguments": "",
                                            }
                                        });
                                        let tc_msg = gladiator_core::Message::new(
                                            &self.stream_topic,
                                            &self.id(),
                                            tc_payload,
                                        )
                                        .with_type("LlmToolCall")
                                        .with_stream_id(stream_id.to_string());
                                        let _ = bus.publish(&self.id(), tc_msg).await;
                                    }
                                    LlmEvent::ToolInputDelta { index, ref name, ref id, text, .. } => {
                                        // Publish incremental tool call progress to TUI.
                                        // `id` may be empty on early deltas before the
                                        // provider sends it; fall back to index for matching.
                                        let tc_payload = serde_json::json!({
                                            "index": index,
                                            "id": id,
                                            "function": {
                                                "name": name,
                                                "arguments": text,
                                            }
                                        });
                                        let tc_msg = gladiator_core::Message::new(
                                            &self.stream_topic,
                                            &self.id(),
                                            tc_payload,
                                        )
                                        .with_type("LlmToolCall")
                                        .with_stream_id(stream_id.to_string());
                                        let _ = bus.publish(&self.id(), tc_msg).await;
                                    }
                                    LlmEvent::ToolInputEnd { index, ref id, name, input } => {
                                        // Publish final tool call arguments to TUI so the
                                        // [tool] placeholder shows complete args before the
                                        // agent dispatches. This lets the display transition
                                        // from streaming deltas to a finalized view.
                                        let tc_payload = serde_json::json!({
                                            "index": index,
                                            "id": id,
                                            "function": {
                                                "name": name,
                                                "arguments": input,
                                            }
                                        });
                                        let tc_msg = gladiator_core::Message::new(
                                            &self.stream_topic,
                                            &self.id(),
                                            tc_payload,
                                        )
                                        .with_type("LlmToolCall")
                                        .with_stream_id(stream_id.to_string());
                                        let _ = bus.publish(&self.id(), tc_msg).await;
                                    }
                                    LlmEvent::ToolCall { id, name, input } => {
                                        tool_calls.push(serde_json::json!({
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": input,
                                            }
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                            // Break out of payload processing if the loop detector fired.
                            if matches!(signal, StreamSignal::StuckLoop) {
                                break;
                            }
                        }
                    }
                    // Break out of the outer stream loop if stuck.
                    if matches!(signal, StreamSignal::StuckLoop) {
                        break;
                    }
                }
                Ok(Some(Err(e))) => {
                    self.publish_stream_end_and_stats(
                        bus,
                        stream_id,
                        rx_chars,
                        state.usage.clone(),
                        config.context_window,
                    )
                    .await;
                    return Err(crate::error::LlmError::StreamInterrupted(
                        full_response.len(),
                        e.to_string(),
                    ));
                }
                Ok(None) => break,
                Err(_) => {
                    let idle = last_activity.elapsed() >= IDLE_TIMEOUT;
                    self.publish_stream_end_and_stats(
                        bus,
                        stream_id,
                        rx_chars,
                        state.usage.clone(),
                        config.context_window,
                    )
                    .await;
                    if idle {
                        signal = StreamSignal::Idle;
                        info!(
                            "[llm] idle timeout (no tokens for {:?}); returning partial response ({} chars)",
                            IDLE_TIMEOUT,
                            full_response.len()
                        );
                        break;
                    }
                    return Err(crate::error::LlmError::StreamInterrupted(
                        full_response.len(),
                        "stream timeout".to_string(),
                    ));
                }
            }
        }

        self.publish_stream_end_and_stats(
            bus,
            stream_id,
            rx_chars,
            state.usage.clone(),
            config.context_window,
        )
        .await;

        // Rescue path: some models emit their native tool-call markup as
        // literal text (usually inside the reasoning channel) instead of
        // structured tool_calls, which the server then fails to parse. When
        // the stream produced no structured calls, scan the accumulated text
        // for well-formed blocks naming tools we actually offered.
        if state.tool_calls.is_empty() {
            if let Some(tools) = offered_tools {
                let known: std::collections::HashSet<String> = tools
                    .iter()
                    .filter_map(|t| t["function"]["name"].as_str().map(|s| s.to_string()))
                    .collect();
                let mut rescued = crate::tool_call_rescue::extract_tool_calls(&full_response, &known);
                if rescued.is_empty() {
                    rescued = crate::tool_call_rescue::extract_tool_calls(&reasoning_response, &known);
                }
                if !rescued.is_empty() {
                    info!(
                        "[llm{}] rescued {} tool call(s) emitted as text instead of structured tool_calls",
                        self.index,
                        rescued.len()
                    );
                    state.tool_calls = rescued;
                    // The turn now proceeds as a normal tool round-trip; don't
                    // let a loop/idle signal divert it into triage.
                    signal = StreamSignal::Normal;
                }
            }
        }

        // When the model got stuck in a reasoning loop, the triage path needs
        // to see what was repeated — but full_response only has TextDelta.
        // Merge accumulated reasoning into the payload so the agent's triage
        // llm_call has the actual loop content to summarize.
        let payload = if matches!(signal, StreamSignal::StuckLoop) && !reasoning_response.is_empty() {
            if full_response.is_empty() {
                format!("[reasoning]\n{}", reasoning_response)
            } else {
                format!("[output]\n{}\n\n[reasoning]\n{}", full_response, reasoning_response)
            }
        } else {
            full_response
        };

        Ok((payload, state.tool_calls, signal))
    }

    async fn send_request(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        tools: Option<&[serde_json::Value]>,
        grammar: Option<&str>,
        bus: &gladiator_core::Bus,
        tool_runtime: Arc<Mutex<ToolRuntime>>,
    ) -> Result<(String, Vec<serde_json::Value>, StreamSignal), Box<dyn std::error::Error + Send + Sync>> {
        if config.model.is_empty() {
            return Err("LLM model name is empty".into());
        }
        info!("[llm] send_request: model={}, tools={}", config.model, tools.is_some());
        if let Some(tools) = tools {
            info!("[llm] tools count: {}", tools.len());
        }

        let stream_id = uuid::Uuid::new_v4().to_string();
        let canonical = CanonicalRequest {
            model: config.model.clone(),
            messages: messages.to_vec(),
            tools: tools.map(|t| t.to_vec()),
            tool_choice: None,
            temperature: Some(config.temperature),
            max_tokens: Some(config.max_tokens),
            stream: true,
            grammar: grammar.map(|s| s.to_string()),
        };

        let provider = ProviderConfig::new("openai", &config.base_url, &config.api_key);
        let route = provider.openai_chat_route();
        info!("[llm] sending request to {} via {}", config.base_url, route.id);
        match route.send(&canonical, config).await {
            Ok(response) => {
            info!("[llm] request succeeded, streaming response");
                self.stream_response(response, config, bus, &stream_id, &*route.protocol, tools, &tool_runtime)
                    .await
                    .map_err(|e| {
                        tracing::error!("[llm{}] Stream failed: {}", self.index, e);
                        Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                    })
            }
            Err(e) => {
            error!("[llm] request failed: {}", e);
                Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            }
        }
    }

    pub async fn generate_object(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        schema: serde_json::Value,
        _bus: &gladiator_core::Bus,
        _tool_runtime: Arc<Mutex<ToolRuntime>>,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let generate_obj = GenerateObject::new(schema);
        let tool_def = generate_obj.tool_definition();
        let tool_choice = generate_obj.force_tool_choice();

        let canonical = CanonicalRequest {
            model: config.model.clone(),
            messages: messages.to_vec(),
            tools: Some(vec![serde_json::to_value(&tool_def).unwrap()]),
            tool_choice: Some(tool_choice),
            temperature: Some(config.temperature),
            max_tokens: Some(config.max_tokens),
            stream: true,
            grammar: None,
        };

        let provider = ProviderConfig::new("openai", &config.base_url, &config.api_key);
        let route = provider.openai_chat_route();
        let response = route.send(&canonical, config).await?;

        let mut state = StreamState::default();
        let mut stream = response.bytes_stream();
        let stream_timeout = std::time::Duration::from_secs(config.stream_timeout_secs);
        let mut events = Vec::new();

        loop {
            match tokio::time::timeout(stream_timeout, stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    let payloads = crate::framing::decode_sse_chunk(&chunk);
                    for payload in payloads {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&payload) {
                            let parsed_events = route.protocol.parse_event(&json, &mut state);
                            events.extend(parsed_events);
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    return Err(format!("Stream error: {}", e).into());
                }
                Ok(None) => break,
                Err(_) => {
                    return Err("Stream timeout".into());
                }
            }
        }

        Ok(generate_obj.parse_response(&events)?)
    }
}

#[async_trait::async_trait]
impl Actor for LlmActor {
    fn id(&self) -> gladiator_core::ActorId {
        format!("gladiator-llm-{}", self.index)
    }

    fn announce(&self) -> gladiator_core::ActorAnnouncement {
        gladiator_core::ActorAnnouncement {
            id: self.id(),
            subscriptions: vec![self.input_topic.clone(), self.control_topic.clone()],
            publications: vec![
                self.output_topic.clone(),
                self.stream_topic.clone(),
                self.stats_topic.clone(),
                self.tool_calls_topic.clone(),
            ],
        }
    }

    async fn run(&self, bus: &gladiator_core::Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut input_rx = bus
            .subscribe(&self.id(), &self.input_topic)
            .await
            .map_err(|e| format!("Failed to subscribe to input topic '{}': {}", self.input_topic, e))?;

        let mut control_rx = bus
            .subscribe(&self.id(), &self.control_topic)
            .await
            .map_err(|e| format!("Failed to subscribe to control topic '{}': {}", self.control_topic, e))?;

        tracing::info!("[llm{}] Listening on input='{}' control='{}'", self.index, self.input_topic, self.control_topic);

        let mut active_request: Option<tokio::task::JoinHandle<Result<(String, Vec<serde_json::Value>, StreamSignal), Box<dyn std::error::Error + Send + Sync>>>> = None;

        loop {
            if let Some(req_handle) = active_request.take() {
                let abort_handle = req_handle.abort_handle();
                let mut req_handle_opt = Some(req_handle);
                loop {
                    let control_msg = tokio::select! {
                        control_msg = control_rx.recv() => control_msg,
                        request_result = async {
                            if let Some(handle) = req_handle_opt.take() {
                                handle.await
                            } else {
                                std::future::pending().await
                            }
                        } => {
                                match request_result {
                                    Ok(Ok((full_response, mut tool_calls, signal))) => {
                                        if signal != StreamSignal::Normal && tool_calls.is_empty() {
                                            // Model went idle OR got stuck in a loop. Signal the
                                            // agent so it can triage and re-inject guidance. Payload
                                            // is the partial response accumulated so far.
                                            let msg_type = match signal {
                                                StreamSignal::Idle => "LlmIdleTimeout",
                                                StreamSignal::StuckLoop => "LlmStuckLoop",
                                                StreamSignal::Normal => unreachable!(),
                                            };
                                            let signal_msg = gladiator_core::Message::new(
                                                &self.output_topic,
                                                &self.id(),
                                                full_response,
                                            )
                                            .with_type(msg_type);
                                            let _ = bus.publish(&self.id(), signal_msg).await;
                                        } else if !tool_calls.is_empty() {
                                            // Tool call arguments arrive as a concatenation of streamed
                                            // string deltas; a dropped chunk can leave the accumulated
                                            // arguments as truncated JSON. Try to repair in place before
                                            // publishing so subscribers always see parseable args when
                                            // recovery is possible.
                                            let repaired = crate::args_fixer::repair_tool_calls(&mut tool_calls);
                                            if repaired > 0 {
                                                tracing::warn!(
                                                    "[llm{}] repaired {} tool call argument payload(s) that failed JSON validation",
                                                    self.index,
                                                    repaired
                                                );
                                            }
                                            let tc_msg = gladiator_core::Message::new(
                                                &self.tool_calls_topic,
                                                &self.id(),
                                                serde_json::to_value(&tool_calls).unwrap(),
                                            ).with_type("LlmToolCalls");
                                            let _ = bus.publish(&self.id(), tc_msg).await;
                                        } else {
                                        let out_msg = gladiator_core::Message::new(
                                            &self.output_topic,
                                            &self.id(),
                                            full_response,
                                        );
                                        let _ = bus.publish(&self.id(), out_msg).await;
                                    }
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("[llm{}] Request failed: {}", self.index, e);
                                    let error_msg = gladiator_core::Message::new(
                                        &self.output_topic,
                                        &self.id(),
                                        format!("Error: {}", e),
                                    ).with_type("LlmError");
                                    let _ = bus.publish(&self.id(), error_msg).await;
                                }
                                Err(e) => {
                                    tracing::error!("[llm{}] Request task was aborted: {}", self.index, e);
                                }
                            }
                            break;
                        }
                    };

                    match control_msg {
                        Ok(msg) => {
                            if let Ok(v) = serde_json::from_value::<serde_json::Value>(msg.payload.clone()) {
                                if let Some(type_str) = v.get("type").and_then(|t| t.as_str()) {
                                    if type_str == "interrupt" {
                                        let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("interrupted");
                                        tracing::info!("[llm{}] Interrupt: {}", self.index, reason);
                                        abort_handle.abort();
                                        if let Some(handle) = req_handle_opt.take() {
                                            let _ = handle.await;
                                        }
                                        let error_msg = gladiator_core::Message::new(
                                            &self.output_topic,
                                            &self.id(),
                                            format!("Interrupted: {}", reason),
                                        );
                                        let _ = bus.publish(&self.id(), error_msg).await;
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                    if let Some(handle) = req_handle_opt.take() {
                        active_request = Some(handle);
                    }
                    break;
                }
            } else {
                tokio::select! {
                    result = input_rx.recv() => {
                        match result {
                            Ok(msg) => {
                                let request: LlmRequest = match serde_json::from_value(msg.payload.clone()) {
                                    Ok(r) => r,
                                    Err(e) => {
                                        tracing::error!("[llm{}] Failed to parse LLM request: {}", self.index, e);
                                        continue;
                                    }
                                };

                                let config = merge_config(&self.config, request.config.as_ref());
                                let messages_vec: Vec<serde_json::Value> = if let Some(ref messages) = request.messages {
                                    messages.clone()
                                } else {
                                    vec![serde_json::json!({"role": "user", "content": request.prompt})]
                                };
                                let tools_vec = request.tools.clone();
                                let grammar_str = request.grammar.clone();
                                let bus_clone = bus.clone();
                                let stream_topic = self.stream_topic.clone();
                                let stats_topic = self.stats_topic.clone();
                                let tool_calls_topic = self.tool_calls_topic.clone();
                                let index = self.index;
                                let config_merged = config.clone();
                                let messages_clone = messages_vec.clone();
                                let tool_runtime_clone = self.tool_runtime.clone();

                                active_request = Some(tokio::spawn(async move {
                                    let llm = LlmActor {
                                        index,
                                        input_topic: String::new(),
                                        output_topic: String::new(),
                                        stream_topic,
                                        stats_topic,
                                        tool_calls_topic,
                                        control_topic: String::new(),
                                        config: config_merged,
                                        tool_runtime: tool_runtime_clone,
                                    };
                                    let tools = tools_vec.as_deref();
                                    let grammar = grammar_str.as_deref();
                                    llm.send_request(&llm.config, &messages_clone, tools, grammar, &bus_clone, llm.tool_runtime.clone()).await
                                }));
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    control_msg = control_rx.recv() => {
                        match control_msg {
                            Ok(msg) => {
                                if let Ok(v) = serde_json::from_value::<serde_json::Value>(msg.payload.clone()) {
                                    if let Some(type_str) = v.get("type").and_then(|t| t.as_str()) {
                                        if type_str == "interrupt" {
                                            let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("interrupted");
                                            let error_msg = gladiator_core::Message::new(
                                                &self.output_topic,
                                                &self.id(),
                                                format!("Interrupted: {}", reason),
                                            );
                                            let _ = bus.publish(&self.id(), error_msg).await;
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
