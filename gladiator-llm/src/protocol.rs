use crate::event::LlmEvent;
use crate::request::CanonicalRequest;

pub trait Protocol: Send + Sync {
    fn id(&self) -> &str;
    fn build_body(&self, request: &CanonicalRequest) -> serde_json::Value;
    fn parse_event(
        &self,
        raw: &serde_json::Value,
        state: &mut StreamState,
    ) -> Vec<LlmEvent>;
    fn terminal_event(&self, state: &StreamState) -> bool;
}

#[derive(Debug, Default)]
pub struct StreamState {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub usage: Option<crate::event::Usage>,
    pub finish_reason: Option<String>,
}
