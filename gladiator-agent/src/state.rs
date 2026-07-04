use crate::internal_tools::{render_todos, TodoEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConversationState {
    pub messages: Vec<serde_json::Value>,
    pub iteration_count: u32,
    pub pending_tool_calls: HashSet<String>,
    pub pending_messages: Vec<String>,
    pub was_interrupted: bool,
    /// Transient agent-internal todo list. Saved/restored with the rest of the
    /// conversation state (not a separate disk file). Manipulated by the
    /// internal `todo_write` / `todo_read` tools, handled inline by the agent.
    #[serde(default)]
    pub todos: Vec<TodoEntry>,
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
    /// Last reported token usage from the LLM (`StreamStats.usage`),
    /// published at end-of-stream by the LLM actor. Transient: not
    /// serialized — refreshed each turn from the live bus.
    #[serde(skip)]
    pub last_usage: Option<Usage>,
    /// Model context window in tokens, when known. Transient: not
    /// serialized — populated at agent startup from LlmConfig.
    #[serde(skip)]
    pub context_window: Option<usize>,
}

/// Subset of `gladiator_llm::Usage` mirrored locally so the agent crate
/// doesn't need a direct dep on gladiator-llm just for the field shape.
/// Constructed from a `StreamStats` payload at the bus boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
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

    /// Replace the entire todo list with `entries`. Returns the rendered
    /// summary that callers can feed back as the tool result.
    pub fn set_todos(&mut self, entries: Vec<TodoEntry>) -> String {
        self.todos = entries;
        render_todos(&self.todos)
    }

    /// Rendered snapshot of the current todo list (empty-safe).
    pub fn todos_render(&self) -> String {
        render_todos(&self.todos)
    }

    /// Record the latest per-turn usage stats. Called when a `StreamStats`
    /// message arrives on the bus. Also updates `context_window` if the
    /// stats carry one (so a runtime probe by the LLM actor propagates here).
    pub fn record_usage(
        &mut self,
        usage: Usage,
        context_window: Option<usize>,
    ) {
        self.last_usage = Some(usage);
        if let Some(w) = context_window {
            self.context_window = Some(w);
        }
    }

    /// Tokens remaining in the context window, computed from the last reported
    /// usage. `None` when either piece is unknown.
    pub fn context_remaining(&self) -> Option<u64> {
        let window = self.context_window? as u64;
        let used = self
            .last_usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0);
        window.checked_sub(used)
    }

    /// Human-readable one-line context status for tool results / logs.
    pub fn context_status_line(&self) -> String {
        match (self.context_remaining(), &self.last_usage) {
            (Some(remaining), Some(u)) => {
                let used = u.input_tokens.unwrap_or(0);
                let pct = if self.context_window.unwrap_or(0) > 0 {
                    (used as f64 / self.context_window.unwrap() as f64) * 100.0
                } else {
                    0.0
                };
                format!(
                    "context: {}/{} tokens used ({:.0}%), {} remaining",
                    used, self.context_window.unwrap(), pct, remaining
                )
            }
            (None, Some(u)) => format!(
                "context: {} input tokens used (window unknown)",
                u.input_tokens.unwrap_or(0)
            ),
            _ => "context: no usage reported yet".to_string(),
        }
    }

    /// Wipe the entire conversation context: messages, todos, pending tool
    /// calls, pending user messages, iteration counter, and interrupt flag.
    /// Used by the `restart_from_file` internal tool after a backup snapshot
    /// has been written to disk, so the next injected user message starts a
    /// clean transcript. Transient accumulator fields (reasoning / partial
    /// response / tool_call_order) are cleared too so nothing leaks across the
    /// restart boundary.
    pub fn clear_for_restart(&mut self) {
        self.messages.clear();
        self.todos.clear();
        self.pending_tool_calls.clear();
        self.pending_messages.clear();
        self.iteration_count = 0;
        self.was_interrupted = false;
        self.current_reasoning.clear();
        self.current_partial_response.clear();
        self.tool_call_order.clear();
    }
}
