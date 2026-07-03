use serde::{Deserialize, Serialize};
use gladiator_core::Actor;
use futures::StreamExt;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("Network error: {0}")]
    Network(String),
    #[error("Request timeout after {0} seconds")]
    Timeout(u64),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("Stream error: {0}")]
    Stream(String),
    #[error("Stream interrupted: partial response ({0} chars), last error: {1}")]
    StreamInterrupted(usize, String),
    #[error("Other: {0}")]
    Other(String),
}

impl LlmError {
    pub fn is_retryable(&self) -> bool {
        match self {
            LlmError::Network(_) => true,
            LlmError::Timeout(_) => true,
            LlmError::Api { status, .. } => *status == 429 || *status >= 500,
            LlmError::Stream(_) => true,
            LlmError::StreamInterrupted(_, _) => false,
            LlmError::Other(_) => false,
        }
    }
}

fn is_retryable_reqwest_error(err: &reqwest::Error) -> bool {
    if err.is_timeout() || err.is_connect() {
        return true;
    }
    if let Some(status) = err.status() {
        return status.as_u16() == 429 || status.as_u16() >= 500;
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    #[serde(default)]
    pub messages: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub config: Option<gladiator_core::LlmConfig>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub grammar: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStats {
    pub rx_chars: usize,
}

struct ToolCallAccumulator {
    name: String,
    args: String,
    call_id: String,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self {
            name: String::new(),
            args: String::new(),
            call_id: String::new(),
        }
    }

    fn push_name(&mut self, name: &str) {
        self.name.push_str(name);
    }

    fn push_args(&mut self, args: &str) {
        self.args.push_str(args);
    }

    fn push_call_id(&mut self, call_id: &str) {
        self.call_id.push_str(call_id);
    }
}

pub use self::merge_config as merge_config_public;

pub fn merge_config(
    base: &gladiator_core::LlmConfig,
    request_config: Option<&gladiator_core::LlmConfig>,
) -> gladiator_core::LlmConfig {
    match request_config {
        Some(req) => gladiator_core::LlmConfig {
            model: if req.model.is_empty() {
                base.model.clone()
            } else {
                req.model.clone()
            },
            base_url: if req.base_url.is_empty() {
                base.base_url.clone()
            } else {
                req.base_url.clone()
            },
            api_key: if req.api_key.is_empty() {
                base.api_key.clone()
            } else {
                req.api_key.clone()
            },
            temperature: req.temperature,
            max_tokens: if req.max_tokens == 0 {
                base.max_tokens
            } else {
                req.max_tokens
            },
            request_timeout_secs: if req.request_timeout_secs == 0 {
                base.request_timeout_secs
            } else {
                req.request_timeout_secs
            },
            stream_timeout_secs: if req.stream_timeout_secs == 0 {
                base.stream_timeout_secs
            } else {
                req.stream_timeout_secs
            },
            max_retries: if req.max_retries == 0 {
                base.max_retries
            } else {
                req.max_retries
            },
            retry_base_delay_ms: if req.retry_base_delay_ms == 0 {
                base.retry_base_delay_ms
            } else {
                req.retry_base_delay_ms
            },
        },
        None => base.clone(),
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
        }
    }

    pub fn build_request_body(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        tools: Option<&[serde_json::Value]>,
        grammar: Option<&str>,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": config.model,
            "messages": messages,
            "temperature": config.temperature,
            "max_tokens": config.max_tokens,
            "stream": true
        });
        if let Some(tools) = tools {
            body["tools"] = serde_json::to_value(tools).unwrap();
        }
        if let Some(grammar_str) = grammar {
            body["grammar"] = serde_json::json!(grammar_str);
        }
        body
    }

    async fn send_http_request_with_retry(
        &self,
        config: &gladiator_core::LlmConfig,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, LlmError> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| LlmError::Other(format!("Failed to create HTTP client: {}", e)))?;

        let mut attempt = 0u32;
        let mut delay = std::time::Duration::from_millis(config.retry_base_delay_ms);

        loop {
            let request = client
                .post(format!("{}/chat/completions", config.base_url))
                .header("Authorization", format!("Bearer {}", config.api_key))
                .header("Content-Type", "application/json")
                .json(body);

            match tokio::time::timeout(
                std::time::Duration::from_secs(config.request_timeout_secs),
                request.send(),
            )
            .await
            {
                Ok(Ok(response)) => {
                    if !response.status().is_success() {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        let err = LlmError::Api {
                            status: status.as_u16(),
                            body,
                        };
                        if err.is_retryable() && attempt < config.max_retries {
                            tracing::warn!(
                                "[llm{}] Request failed (attempt {}/{}, status {}), retrying in {:?}",
                                self.index,
                                attempt + 1,
                                config.max_retries,
                                status.as_u16(),
                                delay
                            );
                            tokio::time::sleep(delay).await;
                            delay = delay.saturating_mul(2);
                            attempt += 1;
                            continue;
                        }
                        return Err(err);
                    }
                    return Ok(response);
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if is_retryable_reqwest_error(&e) && attempt < config.max_retries {
                        tracing::warn!(
                            "[llm{}] Request failed (attempt {}/{}), retrying in {:?}: {}",
                            self.index,
                            attempt + 1,
                            config.max_retries,
                            delay,
                            err_str
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2);
                        attempt += 1;
                        continue;
                    }
                    return Err(LlmError::Network(err_str));
                }
                Err(_) => {
                    if attempt < config.max_retries {
                        tracing::warn!(
                            "[llm{}] Request timed out after {}s (attempt {}/{}), retrying in {:?}",
                            self.index,
                            config.request_timeout_secs,
                            attempt + 1,
                            config.max_retries,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2);
                        attempt += 1;
                        continue;
                    }
                    return Err(LlmError::Timeout(config.request_timeout_secs));
                }
            }
        }
    }

    async fn publish_stream_end_and_stats(&self, bus: &gladiator_core::Bus, stream_id: &str, rx_chars: usize) {
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
    ) -> Result<(String, Vec<serde_json::Value>), LlmError> {
        let mut full_response = String::new();
        let mut stream = response.bytes_stream();
        let mut rx_chars: usize = 0;
        let mut tool_calls: Vec<serde_json::Value> = Vec::new();
        let mut tool_call_accumulators: std::collections::HashMap<usize, ToolCallAccumulator> =
            std::collections::HashMap::new();
        let stream_timeout = std::time::Duration::from_secs(config.stream_timeout_secs);

        loop {
            match tokio::time::timeout(stream_timeout, stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    let text = String::from_utf8_lossy(&chunk);
                    for line in text.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                                if let Some(reasoning) =
                                    json["choices"][0]["delta"]["reasoning_content"].as_str()
                                {
                                    if !reasoning.is_empty() {
                                        rx_chars += reasoning.chars().count();
                                        let chunk_msg = gladiator_core::Message::new(
                                            &self.stream_topic,
                                            &self.id(),
                                            reasoning.to_string(),
                                        )
                                        .with_type("LlmThinking")
                                        .with_stream_id(stream_id.to_string());
                                        let _ = bus.publish(&self.id(), chunk_msg).await;
                                    }
                                }
                                if let Some(content) =
                                    json["choices"][0]["delta"]["content"].as_str()
                                {
                                    if !content.is_empty() {
                                        rx_chars += content.chars().count();
                                        let chunk_msg = gladiator_core::Message::new(
                                            &self.stream_topic,
                                            &self.id(),
                                            content.to_string(),
                                        )
                                        .with_type("LlmStream")
                                        .with_stream_id(stream_id.to_string());
                                        let _ = bus.publish(&self.id(), chunk_msg).await;
                                        full_response.push_str(content);
                                    }
                                }
                                if let Some(tool_calls_arr) =
                                    json["choices"][0]["delta"]["tool_calls"].as_array()
                                {
                                    for tool_call in tool_calls_arr {
                                        if let Some(index) = tool_call["index"].as_u64() {
                                            let idx = index as usize;
                                            let name = tool_call["function"]["name"]
                                                .as_str()
                                                .unwrap_or("");
                                            let args = tool_call["function"]["arguments"]
                                                .as_str()
                                                .unwrap_or("");
                                            let call_id =
                                                tool_call["id"].as_str().unwrap_or("");
                                            let entry = tool_call_accumulators
                                                .entry(idx)
                                                .or_insert_with(ToolCallAccumulator::new);
                                            if !name.is_empty() {
                                                entry.push_name(name);
                                            }
                                            if !args.is_empty() {
                                                entry.push_args(args);
                                            }
                                            if !call_id.is_empty() {
                                                entry.push_call_id(call_id);
                                            }
                                            let tc_msg = gladiator_core::Message::new(
                                                &self.tool_calls_topic,
                                                &self.id(),
                                                serde_json::json!({
                                                    "index": idx,
                                                    "id": entry.call_id,
                                                    "function": {
                                                        "name": entry.name,
                                                        "arguments": entry.args,
                                                    }
                                                }),
                                            )
                                            .with_type("LlmToolCall")
                                            .with_stream_id(stream_id.to_string());
                                            let _ = bus.publish(&self.id(), tc_msg).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;
                    return Err(LlmError::StreamInterrupted(
                        full_response.len(),
                        e.to_string(),
                    ));
                }
                Ok(None) => break,
                Err(_) => {
                    self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;
                    return Err(LlmError::StreamInterrupted(
                        full_response.len(),
                        "stream timeout".to_string(),
                    ));
                }
            }
        }

        self.publish_stream_end_and_stats(bus, stream_id, rx_chars).await;

        for (_idx, entry) in &tool_call_accumulators {
            let full_tc = serde_json::json!({
                "id": entry.call_id,
                "type": "function",
                "function": {
                    "name": entry.name,
                    "arguments": entry.args,
                }
            });
            tool_calls.push(full_tc);
        }

        Ok((full_response, tool_calls))
    }

    async fn send_request(
        &self,
        config: &gladiator_core::LlmConfig,
        messages: &[serde_json::Value],
        tools: Option<&[serde_json::Value]>,
        grammar: Option<&str>,
        bus: &gladiator_core::Bus,
    ) -> Result<(String, Vec<serde_json::Value>), Box<dyn std::error::Error + Send + Sync>> {
        if config.model.is_empty() {
            return Err("LLM model name is empty".into());
        }

        let stream_id = uuid::Uuid::new_v4().to_string();
        let body = self.build_request_body(config, messages, tools, grammar);
        let response = self.send_http_request_with_retry(config, &body).await?;

        self.stream_response(response, config, bus, &stream_id)
            .await
            .map_err(|e| {
                tracing::error!("[llm{}] Stream failed: {}", self.index, e);
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })
    }
}

#[async_trait::async_trait]
impl gladiator_core::Actor for LlmActor {
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
                                Ok(Ok((full_response, tool_calls))) => {
                                    if !tool_calls.is_empty() {
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
                                    };
                                    let tools = tools_vec.as_deref();
                                    let grammar = grammar_str.as_deref();
                                    llm.send_request(&llm.config, &messages_clone, tools, grammar, &bus_clone).await
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
