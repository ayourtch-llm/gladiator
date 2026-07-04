use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmEvent {
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextEnd {
        id: String,
    },
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        text: String,
    },
    ReasoningEnd {
        id: String,
    },
    ToolInputStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        name: String,
        text: String,
    },
    ToolInputEnd {
        id: String,
        name: String,
        input: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Finish {
        reason: String,
        usage: Option<Usage>,
    },
    ProviderError {
        message: String,
        retryable: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub events: Vec<LlmEvent>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

impl LlmResponse {
    pub fn reduce(&mut self, event: LlmEvent) {
        self.events.push(event.clone());
        match event {
            LlmEvent::TextDelta { text, .. } => {
                self.text.push_str(&text);
            }
            LlmEvent::ReasoningDelta { text, .. } => {
                self.reasoning.push_str(&text);
            }
            LlmEvent::ToolCall { id, name, input } => {
                let tc = serde_json::json!({
                    "id": id,
                    "name": name,
                    "input": input,
                });
                self.tool_calls.push(tc);
            }
            LlmEvent::Finish { reason, usage } => {
                self.finish_reason = Some(reason);
                self.usage = usage;
            }
            _ => {}
        }
    }

    pub fn is_complete(&self) -> bool {
        self.finish_reason.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStats {
    pub rx_chars: usize,
}
