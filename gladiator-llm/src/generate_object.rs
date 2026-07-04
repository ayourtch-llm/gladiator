use crate::event::LlmEvent;
use crate::schema::ToolDefinition;
use serde_json::json;

pub struct GenerateObject {
    pub schema: serde_json::Value,
}

impl GenerateObject {
    pub fn new(schema: serde_json::Value) -> Self {
        Self { schema }
    }

    pub fn tool_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "generate_object".to_string(),
            description: "Return the structured result by calling this tool.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": self.schema,
                "required": []
            }),
        }
    }

    pub fn force_tool_choice(&self) -> serde_json::Value {
        json!({
            "type": "function",
            "function": { "name": "generate_object" }
        })
    }

    pub fn parse_response(&self, events: &[LlmEvent]) -> Result<serde_json::Value, String> {
        let tool_call = events.iter()
            .find(|e| matches!(e, LlmEvent::ToolCall { name, .. } if name == "generate_object"))
            .ok_or_else(|| "No generate_object tool call found".to_string())?;

        match tool_call {
            LlmEvent::ToolCall { input, .. } => {
                serde_json::from_value(input.clone())
                    .map_err(|e| format!("Failed to parse generate_object response: {}", e))
            }
            _ => Err("Expected ToolCall event".to_string()),
        }
    }
}
