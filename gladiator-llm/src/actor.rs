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

        let stats_msg = gladiator_core::Message::new(
            &self.stats_topic,
            &self.id(),
            serde_json::to_value(&StreamStats { rx_chars }).unwrap(),
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
        _tool_runtime: &Arc<Mutex<ToolRuntime>>,
    ) -> Result<(String, Vec<serde_json::Value>), crate::error::LlmError> {
        let mut full_response = String::new();
        let mut stream = response.bytes_stream();
        let mut rx_chars: usize = 0;
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut state = StreamState::default();
        let stream_timeout = std::time::Duration::from_secs(config.stream_timeout_secs);

        loop {
            match tokio::time::timeout(stream_timeout, stream.next()).await {
                Ok(Some(Ok(chunk))) => {
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
                                            debug!("[llm] text delta: {}", &text[..text.len().min(100)]);
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
                                        }
                                    }
                                    LlmEvent::ReasoningDelta { text, .. } => {
                                        if !text.is_empty() {
                                            rx_chars += text.chars().count();
                                            let chunk_msg = gladiator_core::Message::new(
                                                &self.stream_topic,
                                                &self.id(),
                                                text.clone(),
                                            )
                                            .with_type("LlmThinking")
                                            .with_stream_id(stream_id.to_string());
                                            let _ = bus.publish(&self.id(), chunk_msg).await;
                                        }
                                    }
                                    LlmEvent::ToolInputStart { name, .. } => {
                                        // Publish tool call start to TUI for progress display
                                        let tc_payload = serde_json::json!({
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
                                    LlmEvent::ToolInputDelta { name, text, .. } => {
                                        // Publish incremental tool call progress to TUI
                                        let tc_payload = serde_json::json!({
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
                                    LlmEvent::ToolInputEnd { .. } => {
                                        // Tool calls are published after stream ends
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
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;
                    return Err(crate::error::LlmError::StreamInterrupted(
                        full_response.len(),
                        e.to_string(),
                    ));
                }
                Ok(None) => break,
                Err(_) => {
                    self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;
                    return Err(crate::error::LlmError::StreamInterrupted(
                        full_response.len(),
                        "stream timeout".to_string(),
                    ));
                }
            }
        }

        self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;

        Ok((full_response, state.tool_calls))
    }

    async fn send_request(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        tools: Option<&[serde_json::Value]>,
        grammar: Option<&str>,
        bus: &gladiator_core::Bus,
        tool_runtime: Arc<Mutex<ToolRuntime>>,
    ) -> Result<(String, Vec<serde_json::Value>), Box<dyn std::error::Error + Send + Sync>> {
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
                self.stream_response(response, config, bus, &stream_id, &*route.protocol, &tool_runtime)
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
        bus: &gladiator_core::Bus,
        tool_runtime: Arc<Mutex<ToolRuntime>>,
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

        let mut active_request: Option<tokio::task::JoinHandle<Result<(String, Vec<serde_json::Value>), Box<dyn std::error::Error + Send + Sync>>>> = None;

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
                                    Ok(Ok((full_response, mut tool_calls))) => {
                                        if !tool_calls.is_empty() {
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
                                    );
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
