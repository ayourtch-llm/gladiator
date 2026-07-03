use crate::state::ConversationState;
use gladiator_core::{Actor, ActorAnnouncement, AgentConfig, Bus, Message};
use gladiator_llm::{LlmRequest, merge_config};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::broadcast::error::RecvError;
use tracing::{error, info, warn};

#[derive(Debug, Default)]
pub struct AgentActor {
    pub index: usize,
    pub input_topic: String,
    pub llm_in_topic: String,
    pub llm_out_topic: String,
    pub llm_stream_topic: String,
    pub llm_tool_calls_topic: String,
    pub tool_results_topic: String,
    pub stream_output_topic: String,
    pub config: AgentConfig,
    pub max_iterations: u32,
    pub system_message: String,
    pub tool_defs: Vec<serde_json::Value>,
    pub tool_timeout_secs: u64,
}

impl AgentActor {
    pub fn new(
        index: usize,
        input_topic: String,
        llm_in_topic: String,
        llm_out_topic: String,
        llm_stream_topic: String,
        llm_tool_calls_topic: String,
        tool_results_topic: String,
        stream_output_topic: String,
        config: AgentConfig,
    ) -> Self {
        Self {
            index,
            input_topic,
            llm_in_topic,
            llm_out_topic,
            llm_stream_topic,
            llm_tool_calls_topic,
            tool_results_topic,
            stream_output_topic,
            max_iterations: config.max_iterations,
            system_message: config.system_message.clone(),
            config,
            tool_defs: Vec::new(),
            tool_timeout_secs: 300,
        }
    }

    pub fn with_max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn with_system_message(mut self, msg: String) -> Self {
        self.system_message = msg;
        self
    }

    pub fn with_tool_defs(mut self, defs: Vec<serde_json::Value>) -> Self {
        self.tool_defs = defs;
        self
    }

    pub fn with_tool_timeout_secs(mut self, secs: u64) -> Self {
        self.tool_timeout_secs = secs;
        self
    }

    async fn send_conversation(
        &self,
        bus: &Bus,
        messages: &[serde_json::Value],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let llm_request = LlmRequest {
            messages: Some(messages.to_vec()),
            prompt: String::new(),
            config: None,
            tools: if self.tool_defs.is_empty() {
                None
            } else {
                Some(self.tool_defs.clone())
            },
            grammar: None,
        };

        let msg = Message::new(
            &self.llm_in_topic,
            &self.id(),
            serde_json::to_value(&llm_request)
                .map_err(|e| format!("Failed to serialize LLM request: {}", e))?,
        );

        let mut attempt = 0u32;
        loop {
            match bus.publish(&self.id(), msg.clone()).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if attempt < 3 {
                        error!("Failed to publish to LLM input (attempt {}): {}", attempt + 1, e);
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                        attempt += 1;
                    } else {
                        return Err(format!("Failed to publish to LLM input after 3 attempts: {}", e).into());
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Actor for AgentActor {
    fn id(&self) -> gladiator_core::ActorId {
        format!("gladiator-agent-{}", self.index)
    }

    fn announce(&self) -> ActorAnnouncement {
        ActorAnnouncement {
            id: self.id(),
            subscriptions: vec![
                self.input_topic.clone(),
                self.llm_out_topic.clone(),
                self.llm_stream_topic.clone(),
                self.llm_tool_calls_topic.clone(),
                self.tool_results_topic.clone(),
            ],
            publications: vec![self.stream_output_topic.clone(), self.llm_in_topic.clone()],
        }
    }

    async fn run(&self, bus: &Bus) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state: Arc<tokio::sync::Mutex<ConversationState>> =
            Arc::new(tokio::sync::Mutex::new(ConversationState::new()));

        let mut input_rx = bus.subscribe(&self.id(), &self.input_topic).await?;
        let mut out_rx = bus.subscribe(&self.id(), &self.llm_out_topic).await?;
        let mut stream_rx = bus.subscribe(&self.id(), &self.llm_stream_topic).await?;
        let mut tool_calls_rx = bus.subscribe(&self.id(), &self.llm_tool_calls_topic).await?;
        let mut tool_results_rx = bus.subscribe(&self.id(), &self.tool_results_topic).await?;

        let mut tool_watchdog = tokio::time::interval(std::time::Duration::from_secs(10));

        info!(
            "Agent actor {} listening on '{}' with {} tools, max_iterations={}",
            self.index,
            self.input_topic,
            self.tool_defs.len(),
            self.max_iterations
        );

        loop {
            tokio::select! {
                result = input_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let user_message = msg.payload_str().unwrap_or_else(|| msg.payload.to_string());

                            {
                                let mut s = state.lock().await;
                                if !s.pending_tool_calls.is_empty() {
                                    s.buffer_user_message(user_message);
                                    continue;
                                }
                                s.add_user_message(user_message);
                            }

                            let messages = {
                                let s = state.lock().await;
                                s.build_messages_with_system(&self.system_message)
                            };
                            if let Err(e) = self.send_conversation(bus, &messages).await {
                                error!("Failed to send conversation: {}", e);
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} input lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = out_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let output = msg.payload_str().unwrap_or_else(|| msg.payload.to_string());
                            {
                                let mut s = state.lock().await;
                                s.add_assistant_message(output);
                                s.increment_iteration();
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} output lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = stream_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let mut forwarded = msg.clone();
                            forwarded.topic = self.stream_output_topic.clone();
                            let _ = bus.publish(&self.id(), forwarded).await;
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} stream lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = tool_calls_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let tool_calls: Vec<serde_json::Value> = match &msg.payload {
                                serde_json::Value::Array(arr) => arr.clone(),
                                serde_json::Value::Object(_) => continue,
                                _ => continue,
                            };

                            {
                                let mut s = state.lock().await;
                                s.add_tool_calls(tool_calls.clone());
                            }

                            for tc in &tool_calls {
                                let tool_call_id = tc["id"].as_str().unwrap_or("").to_string();
                                let func_name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                                let func_args_str = tc["function"]["arguments"].as_str().unwrap_or("");

                                let args: serde_json::Value = match serde_json::from_str(func_args_str) {
                                    Ok(a) => a,
                                    Err(e) => {
                                        error!("Failed to parse tool args for {}: {}", func_name, e);
                                        let mut s = state.lock().await;
                                        s.add_tool_result(&tool_call_id, &func_name, format!("Error parsing arguments: {}", e), false);
                                        s.resolve_tool_call(&tool_call_id);
                                        continue;
                                    }
                                };

                                info!("Agent {} dispatching tool call: {}({})", self.index, func_name, func_args_str);

                                let exec_payload = serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": func_name,
                                    "arguments": args,
                                });

                                let exec_msg = Message::new(
                                    &format!("tool:{}:execute", func_name),
                                    &self.id(),
                                    exec_payload,
                                );
                                let _ = bus.publish(&self.id(), exec_msg).await;
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} tool_calls lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                result = tool_results_rx.recv() => {
                    match result {
                        Ok(msg) => {
                            let tool_result: gladiator_tools::ToolResultMessage = match serde_json::from_value(msg.payload) {
                                Ok(tr) => tr,
                                Err(e) => {
                                    error!("Failed to parse tool result: {}", e);
                                    continue;
                                }
                            };

                            {
                                let mut s = state.lock().await;
                                s.add_tool_result(
                                    &tool_result.tool_call_id,
                                    &tool_result.tool_name,
                                    &tool_result.result,
                                    tool_result.success,
                                );
                                s.resolve_tool_call(&tool_result.tool_call_id);
                            }

                            let result_text = if tool_result.success {
                                tool_result.result.as_str()
                            } else {
                                tool_result.error.as_deref().unwrap_or("unknown")
                            };
                            let stream_msg = Message::new(
                                &self.stream_output_topic,
                                &self.id(),
                                format!("  [tool_{}] {}({}) => {}",
                                    if tool_result.success { "result" } else { "error" },
                                    tool_result.tool_name,
                                    tool_result.tool_call_id,
                                    result_text
                                ),
                            ).with_type("LlmToolResult");
                            let _ = bus.publish(&self.id(), stream_msg).await;

                            {
                                let s = state.lock().await;
                                if s.all_tool_calls_resolved() {
                                    if s.max_reached(self.max_iterations) {
                                        drop(s);
                                        let warn_msg = Message::new(
                                            &self.stream_output_topic,
                                            &self.id(),
                                            format!("Max iterations ({}) reached", self.max_iterations),
                                        ).with_type("Warning");
                                        let _ = bus.publish(&self.id(), warn_msg).await;
                                    } else {
                                        let messages = s.build_messages_with_system(&self.system_message);
                                        drop(s);
                                        if let Err(e) = self.send_conversation(bus, &messages).await {
                                            error!("Failed to send tool results to LLM: {}", e);
                                        }
                                    }
                                }
                            }
                        }
                        Err(RecvError::Lagged(n)) => warn!("Agent {} tool_results lagged: {}", self.index, n),
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = tool_watchdog.tick() => {
                    let s = state.lock().await;
                    if !s.pending_tool_calls.is_empty() {
                        warn!("Agent {} has {} pending tool calls (watchdog tick)", self.index, s.pending_tool_calls.len());
                    }
                }
            }
        }

        Ok(())
    }
}
