use crate::analyzer::ProjectManager;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

pub async fn get_type_hierarchy_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?
        .to_string();
    let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let character = args
        .get("character")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("Missing character parameter"))? as u32;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .get_type_hierarchy(&file_path, line, character)
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}
