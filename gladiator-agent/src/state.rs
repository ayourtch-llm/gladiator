use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct ConversationState {
    pub messages: Vec<serde_json::Value>,
    pub iteration_count: u32,
    pub pending_tool_calls: HashSet<String>,
    pub pending_messages: Vec<String>,
    pub was_interrupted: bool,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
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

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.messages.push(serde_json::json!({
            "role": "assistant",
            "content": content.into()
        }));
    }

    pub fn add_tool_calls(&mut self, tool_calls: Vec<serde_json::Value>) {
        self.messages.push(serde_json::json!({
            "role": "assistant",
            "tool_calls": tool_calls
        }));
        for tc in &tool_calls {
            if let Some(id) = tc["id"].as_str() {
                self.pending_tool_calls.insert(id.to_string());
            }
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
    }

    pub fn all_tool_calls_resolved(&self) -> bool {
        self.pending_tool_calls.is_empty()
    }

    pub fn increment_iteration(&mut self) {
        self.iteration_count += 1;
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
        result.extend(self.messages.iter().cloned());
        result
    }
}
