use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool(String),
}

impl ToolChoice {
    pub fn to_openai_value(&self) -> serde_json::Value {
        match self {
            ToolChoice::Auto => serde_json::json!("auto"),
            ToolChoice::None => serde_json::json!("none"),
            ToolChoice::Required => serde_json::json!("required"),
            ToolChoice::Tool(name) => serde_json::json!({
                "type": "function",
                "function": { "name": name }
            }),
        }
    }
}
