use anyhow::Result;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::sync::Arc;

use crate::analyzer::ProjectManager;

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

/// Execute a tool call. The `manager` holds multiple RA clients keyed by project.
/// Most tools dispatch through the active client; project management tools
/// (add_project, switch_project, list_projects, get_active_project) operate on
/// the manager directly without needing an RA instance.
pub async fn execute_tool(
    name: &str,
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    match name {
        "find_definition" => crate::tools::analysis::find_definition_impl(args, manager).await,
        "find_references" => crate::tools::analysis::find_references_impl(args, manager).await,
        "get_diagnostics" => crate::tools::analysis::get_diagnostics_impl(args, manager).await,
        "workspace_symbols" => {
            crate::tools::navigation::workspace_symbols_impl(args, manager).await
        }
        "rename_symbol" => crate::tools::refactoring::rename_symbol_impl(args, manager).await,
        "extract_function" => {
            crate::tools::refactoring::extract_function_impl(args, manager).await
        }
        "inline_function" => {
            crate::tools::refactoring::inline_function_impl(args, manager).await
        }
        "organize_imports" => {
            crate::tools::refactoring::organize_imports_impl(args, manager).await
        }
        "get_type_hierarchy" => {
            crate::tools::advanced::get_type_hierarchy_impl(args, manager).await
        }
        // Project management tools — operate on the manager directly.
        "add_project" => add_project_impl(args, manager),
        "switch_project" => switch_project_impl(args, manager),
        "list_projects" => list_projects_impl(manager),
        "get_active_project" => get_active_project_impl(manager),
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

// ---- Project management tool implementations ----

fn add_project_impl(args: Value, manager: &mut ProjectManager) -> Result<ToolResult> {
    let project_path = args
        .get("project_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing project_path parameter"))?;
    let key = manager.add_project(project_path)?;
    Ok(ToolResult {
        content: vec![json!({
            "type": "text",
            "text": format!(
                "Registered project '{project_path}' (key={key}). Active project is now '{}'.\n\
                 RA will start lazily on first tool call for this project.",
                manager.get_active_key().unwrap_or_default()
            )
        }).as_object().unwrap().clone()],
    })
}

fn switch_project_impl(args: Value, manager: &mut ProjectManager) -> Result<ToolResult> {
    let project_path = args
        .get("project_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing project_path parameter"))?;
    manager.set_active(project_path)?;
    Ok(ToolResult {
        content: vec![json!({
            "type": "text",
            "text": format!(
                "Switched active project to '{}'.\n\
                 Registered projects ({} total): {}",
                std::fs::canonicalize(project_path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| project_path.to_string()),
                manager.project_count(),
                manager.list_projects()
                    .iter()
                    .map(|(k, active)| if *active { format!("[{}]", k) } else { k.clone() })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }).as_object().unwrap().clone()],
    })
}

fn list_projects_impl(manager: &ProjectManager) -> Result<ToolResult> {
    let entries = manager.list_projects();
    if entries.is_empty() {
        return Ok(ToolResult {
            content: vec![json!({
                "type": "text",
                "text": "No projects registered. Use add_project to register a project root."
            }).as_object().unwrap().clone()],
        });
    }
    let mut lines = Vec::new();
    for (path, is_active) in &entries {
        let marker = if *is_active { "* ACTIVE" } else { "" };
        lines.push(format!("  {path} {marker}"));
    }
    Ok(ToolResult {
        content: vec![json!({
            "type": "text",
            "text": format!(
                "Registered projects ({}):\n{}\n* = active project for tool calls",
                entries.len(),
                lines.join("\n")
            )
        }).as_object().unwrap().clone()],
    })
}

fn get_active_project_impl(manager: &ProjectManager) -> Result<ToolResult> {
    let key = manager.get_active_key();
    Ok(ToolResult {
        content: vec![json!({
            "type": "text",
            "text": match key {
                Some(k) => format!("Active project: {k}"),
                None => "No active project set. Use add_project to register and activate a project.".to_string(),
            }
        }).as_object().unwrap().clone()],
    })
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
        // Project management (multi-project support)
        ToolDefinition::new(
            "add_project",
            "Register a project root directory for rust-analyzer analysis. The RA instance starts lazily on first tool call.",
            json!({
                "type": "object",
                "properties": {
                    "project_path": {"type": "string", "description": "Absolute path to the project root (directory containing Cargo.toml)"}
                },
                "required": ["project_path"]
            }),
        ),
        ToolDefinition::new(
            "switch_project",
            "Switch the active rust-analyzer project. All subsequent tool calls will use this project's RA instance.",
            json!({
                "type": "object",
                "properties": {
                    "project_path": {"type": "string", "description": "Absolute path to a previously registered project root"}
                },
                "required": ["project_path"]
            }),
        ),
        ToolDefinition::new(
            "list_projects",
            "List all registered rust-analyzer projects and show which one is active.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        ToolDefinition::new(
            "get_active_project",
            "Get the currently active rust-analyzer project root path.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
    ]
}
