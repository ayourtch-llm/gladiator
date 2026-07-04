use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone)]
pub struct CanonicalRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
    pub stream: bool,
    pub grammar: Option<String>,
}

impl CanonicalRequest {
    pub fn from_llm_request(req: &LlmRequest) -> Self {
        let messages = if let Some(ref msgs) = req.messages {
            msgs.clone()
        } else {
            vec![serde_json::json!({"role": "user", "content": req.prompt})]
        };

        CanonicalRequest {
            model: req.config.as_ref().map(|c| c.model.clone()).unwrap_or_default(),
            messages,
            tools: req.tools.clone(),
            tool_choice: None,
            temperature: req.config.as_ref().map(|c| c.temperature),
            max_tokens: req.config.as_ref().map(|c| c.max_tokens),
            stream: true,
            grammar: req.grammar.clone(),
        }
    }
}
