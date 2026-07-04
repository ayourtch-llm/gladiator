use crate::tool::{Tool, ToolSyntax};
use std::sync::Arc;
use tracing::warn;

/// Registry that holds all available tools and provides OpenAI-compatible
/// tool definitions for the LLM.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn add(&mut self, tool: Box<dyn Tool>) -> bool {
        let name = tool.name().to_string();
        if self.tools.iter().any(|t| t.name() == name) {
            warn!("Tool '{}' already in registry, skipping duplicate", name);
            return false;
        }
        self.tools.push(Arc::from(tool));
        true
    }

    pub fn add_arc(&mut self, tool: Arc<dyn Tool>) -> bool {
        let name = tool.name().to_string();
        if self.tools.iter().any(|t| t.name() == name) {
            warn!("Tool '{}' already in registry, skipping duplicate", name);
            return false;
        }
        self.tools.push(tool);
        true
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn syntaxes(&self) -> Vec<ToolSyntax> {
        self.tools
            .iter()
            .map(|t| {
                let syntax = ToolSyntax::new(
                    t.name().to_string(),
                    t.description().to_string(),
                    t.parameters(),
                );
                eprintln!("[registry] tool: {} params: {}", syntax.name, serde_json::to_string(&syntax.parameters).unwrap_or_default());
                syntax
            })
            .collect()
    }

    pub fn to_openai_json(&self) -> serde_json::Value {
        self.tools
            .iter()
            .map(|t| {
                ToolSyntax::new(
                    t.name().to_string(),
                    t.description().to_string(),
                    t.parameters(),
                )
                .to_openai_json()
            })
            .collect::<Vec<_>>()
            .into()
    }

    pub fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.iter()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}
