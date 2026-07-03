use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// OpenAI-compatible tool definition (name, description, parameters).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSyntax {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolSyntax {
    pub fn new(name: String, description: String, parameters: serde_json::Value) -> Self {
        Self {
            name,
            description,
            parameters,
        }
    }

    pub fn to_openai_json(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// Message sent to a tool actor to execute a tool call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecuteMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// Message published by a tool actor after execution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub success: bool,
    pub result: String,
    pub error: Option<String>,
}

/// Trait for tools that can be called by the agent.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;
    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String>;

    fn supports_cancel(&self) -> bool {
        false
    }
}
