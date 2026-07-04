use crate::schema::ToolDefinition;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use async_trait::async_trait;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> &serde_json::Value;
    async fn execute(&self, args: serde_json::Value) -> Result<serde_json::Value, String>;
}

pub struct ToolRuntime {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl fmt::Debug for ToolRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRuntime")
            .field("tool_count", &self.tools.len())
            .finish()
    }
}

impl ToolRuntime {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub async fn execute(&self, name: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
        let tool = self.tools.get(name)
            .ok_or_else(|| format!("Tool '{}' not found", name))?;
        tool.execute(args).await
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters().clone(),
        }).collect()
    }
}

impl Default for ToolRuntime {
    fn default() -> Self {
        Self::new()
    }
}
