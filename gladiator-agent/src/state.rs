use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConversationState {
    pub messages: Vec<serde_json::Value>,
    pub iteration_count: u32,
    pub pending_tool_calls: HashSet<String>,
    pub pending_messages: Vec<String>,
    pub was_interrupted: bool,
    /// Accumulated reasoning from LlmThinking stream chunks.
    /// Transient: not serialized (see #[serde(skip)]). Attached to the
    /// next assistant/tool_calls message via "reasoning" field.
    #[serde(skip)]
    pub current_reasoning: String,
    /// Accumulated partial content from LlmStream chunks.
    /// Transient: not serialized. Used to preserve partial assistant
    /// text when the LLM is interrupted mid-response.
    #[serde(skip)]
    pub current_partial_response: String,
    /// Ordered list of tool_call IDs in the current batch. Used to
    /// reorder tool results to match the LLM's tool_calls array.
    /// Transient: not serialized.
    #[serde(skip)]
    pub tool_call_order: Vec<String>,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.current_reasoning.clear();
        let content_str = content.into();
        self.messages.push(serde_json::json!({
            "role": "user",
            "content": content_str
        }));
    }

    /// Merge a user message with the last message if it's also a user message.
    /// If the last message is a user message, append the new content to it
    /// (separated by a newline). Otherwise, add as a new user message.
    /// This is used after an interrupt to avoid sending two user messages
    /// in a row to the LLM.
    pub fn merge_user_message(&mut self, content: impl Into<String>) {
        self.current_reasoning.clear();
        let content_str = content.into();
        if let Some(last) = self.messages.last_mut() {
            if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                if let Some(existing) = last.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) {
                    let new_content = format!("{}\n{}", existing, content_str);
                    last["content"] = serde_json::Value::String(new_content);
                    return;
                }
            }
        }
        // If last message is not a user message, add as new user message
        self.add_user_message(content_str);
    }

    /// Append a chunk of reasoning content accumulated during streaming.
    /// Chunks are concatenated directly (matching TUI streaming behavior).
    pub fn append_reasoning(&mut self, chunk: &str) {
        self.current_reasoning.push_str(chunk);
    }

    /// Append a chunk of partial response content accumulated during streaming.
    pub fn append_partial_response(&mut self, chunk: &str) {
        self.current_partial_response.push_str(chunk);
    }

    /// Drain accumulated partial response, returning it if non-empty.
    pub fn drain_partial_response(&mut self) -> Option<String> {
        if self.current_partial_response.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current_partial_response))
        }
    }

    /// Drain accumulated reasoning, returning it if non-empty.
    fn drain_reasoning(&mut self) -> Option<String> {
        if self.current_reasoning.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.current_reasoning))
        }
    }

    /// Clear any accumulated reasoning (e.g. after an interrupt).
    pub fn clear_reasoning(&mut self) {
        self.current_reasoning.clear();
    }

    /// Clear accumulated partial response (e.g. after an interrupt).
    pub fn clear_partial_response(&mut self) {
        self.current_partial_response.clear();
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        let reasoning = self.drain_reasoning();
        // The committed text supersedes any streamed partial; clear it so it
        // can't leak into a later tool-call turn's content.
        self.clear_partial_response();
        let mut msg = serde_json::json!({
            "role": "assistant",
            "content": content.into()
        });
        if let Some(r) = reasoning {
            msg["reasoning"] = serde_json::Value::String(r);
        }
        self.messages.push(msg);
    }

    pub fn add_tool_calls(&mut self, tool_calls: Vec<serde_json::Value>) {
        let reasoning = self.drain_reasoning();
        // Preserve any natural-language content the model emitted alongside the
        // tool calls (streamed as LlmStream chunks). Without this the assistant
        // turn is pure tool_calls with no words, so the model has no durable
        // record of its own decisions and re-derives them on later turns.
        let content = self.drain_partial_response();
        // Record the order of tool call IDs for reordering results later.
        // Synthesize ids for empty/missing ones so they don't collapse in the HashSet.
        self.tool_call_order.clear();
        for (i, tc) in tool_calls.iter().enumerate() {
            let id = tc["id"].as_str().unwrap_or("");
            let id = if id.is_empty() {
                format!("__idx_{}", i)
            } else {
                id.to_string()
            };
            self.tool_call_order.push(id);
        }
        let mut msg = serde_json::json!({
            "role": "assistant",
            "tool_calls": tool_calls
        });
        if let Some(c) = content {
            if !c.is_empty() {
                msg["content"] = serde_json::Value::String(c);
            }
        }
        if let Some(r) = reasoning {
            msg["reasoning"] = serde_json::Value::String(r);
        }
        self.messages.push(msg);
        for id in &self.tool_call_order {
            self.pending_tool_calls.insert(id.clone());
        }
    }

    pub fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
        success: bool,
    ) {
        let result_content = if success {
            content.into()
        } else {
            format!("Error: {}", content.into())
        };
        self.messages.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id.into(),
            "name": name.into(),
            "content": result_content,
        }));
    }

    pub fn resolve_tool_call(&mut self, tool_call_id: &str) {
        self.pending_tool_calls.remove(tool_call_id);
        // When all tool calls in the current batch are resolved, reorder
        // the tool result messages to match the original tool_calls array
        // order from the LLM.
        if self.pending_tool_calls.is_empty() && !self.tool_call_order.is_empty() {
            self.reorder_tool_results();
        }
    }

    pub fn all_tool_calls_resolved(&self) -> bool {
        self.pending_tool_calls.is_empty()
    }

    /// Reorder the last N tool result messages to match the original
    /// tool_calls array order. Called when all tool calls in a batch
    /// are resolved. N is determined by `tool_call_order.len()`.
    fn reorder_tool_results(&mut self) {
        let n = self.tool_call_order.len();
        if n == 0 || self.messages.len() < n {
            self.tool_call_order.clear();
            return;
        }

        // The last n messages should be the tool results from this batch
        let start = self.messages.len() - n;
        let mut tool_results: Vec<serde_json::Value> = self.messages.drain(start..).collect();

        // Sort tool results to match the original tool_call_order
        let order = self.tool_call_order.clone();
        tool_results.sort_by_key(|result| {
            let tc_id = result
                .get("tool_call_id")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            order.iter().position(|id| id == tc_id).unwrap_or(usize::MAX)
        });

        for result in tool_results {
            self.messages.push(result);
        }

        self.tool_call_order.clear();
    }

    pub fn increment_iteration(&mut self) {
        self.iteration_count += 1;
    }

    /// Reset the iteration counter to zero. Called when a new user message
    /// arrives so the agent gets a fresh iteration budget for the new turn.
    pub fn reset_iteration(&mut self) {
        self.iteration_count = 0;
    }

    pub fn max_reached(&self, max: u32) -> bool {
        self.iteration_count >= max
    }

    pub fn buffer_user_message(&mut self, message: String) {
        self.pending_messages.push(message);
    }

    pub fn drain_pending_messages(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_messages)
    }

    pub fn build_messages_with_system(&self, system_message: &str) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        if !system_message.is_empty() {
            result.push(serde_json::json!({"role": "system", "content": system_message}));
        }
        for msg in &self.messages {
            let mut m = msg.clone();
            if let serde_json::Value::Object(ref mut obj) = m {
                obj.remove("reasoning");
            }
            result.push(m);
        }
        result
    }
}
