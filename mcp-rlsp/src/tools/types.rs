use anyhow::Result;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::Arc;

use crate::analyzer::RustAnalyzerClient;

pub struct ToolDefinition {
    pub name: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub input_schema: Arc<serde_json::Map<String, Value>>,
}

impl ToolDefinition {
    pub fn new(name: &'static str, description: &'static str, schema: Value) -> Self {
        let schema_map = match schema {
            Value::Object(map) => Arc::new(map),
            _ => Arc::new(serde_json::Map::new()),
        };

        Self {
            name: Cow::Borrowed(name),
            description: Cow::Borrowed(description),
            input_schema: schema_map,
        }
    }
}

pub struct ToolResult {
    pub content: Vec<serde_json::Map<String, Value>>,
}

pub async fn execute_tool(
    name: &str,
    args: Value,
    analyzer: &mut RustAnalyzerClient,
) -> Result<ToolResult> {
    match name {
        "find_definition" => crate::tools::analysis::find_definition_impl(args, analyzer).await,
        "find_references" => crate::tools::analysis::find_references_impl(args, analyzer).await,
        "get_diagnostics" => crate::tools::analysis::get_diagnostics_impl(args, analyzer).await,
        "workspace_symbols" => {
            crate::tools::navigation::workspace_symbols_impl(args, analyzer).await
        }
        "rename_symbol" => crate::tools::refactoring::rename_symbol_impl(args, analyzer).await,
        "extract_function" => {
            crate::tools::refactoring::extract_function_impl(args, analyzer).await
        }
        "inline_function" => crate::tools::refactoring::inline_function_impl(args, analyzer).await,
        "organize_imports" => {
            crate::tools::refactoring::organize_imports_impl(args, analyzer).await
        }
        "get_type_hierarchy" => {
            crate::tools::advanced::get_type_hierarchy_impl(args, analyzer).await
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

pub fn get_tools() -> Vec<ToolDefinition> {
    vec![
        // Code Analysis (LSP-backed)
        ToolDefinition::new(
            "find_definition",
            "Find the definition of a symbol at a given position",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "line": {"type": "number"},
                    "character": {"type": "number"}
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        ToolDefinition::new(
            "find_references",
            "Find all references to a symbol at a given position",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "line": {"type": "number"},
                    "character": {"type": "number"}
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        ToolDefinition::new(
            "get_diagnostics",
            "Get compiler diagnostics for a file",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        ),
        ToolDefinition::new(
            "workspace_symbols",
            "Search for symbols in the workspace",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        ),
        // Refactoring (LSP-backed)
        ToolDefinition::new(
            "rename_symbol",
            "Rename a symbol with scope awareness",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "line": {"type": "number"},
                    "character": {"type": "number"},
                    "new_name": {"type": "string"}
                },
                "required": ["file_path", "line", "character", "new_name"]
            }),
        ),
        ToolDefinition::new(
            "extract_function",
            "Extract selected code into a new function",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "start_line": {"type": "number"},
                    "start_character": {"type": "number"},
                    "end_line": {"type": "number"},
                    "end_character": {"type": "number"},
                    "function_name": {"type": "string"}
                },
                "required": ["file_path", "start_line", "start_character", "end_line", "end_character", "function_name"]
            }),
        ),
        ToolDefinition::new(
            "inline_function",
            "Inline a function call at specified position",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "line": {"type": "number"},
                    "character": {"type": "number"}
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
        ToolDefinition::new(
            "organize_imports",
            "Organize and sort import statements in a file",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"}
                },
                "required": ["file_path"]
            }),
        ),
        // Advanced (LSP-backed)
        ToolDefinition::new(
            "get_type_hierarchy",
            "Get type hierarchy for a symbol at specified position",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "line": {"type": "integer", "minimum": 0},
                    "character": {"type": "integer", "minimum": 0}
                },
                "required": ["file_path", "line", "character"]
            }),
        ),
    ]
}
