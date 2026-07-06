use crate::analyzer::ProjectManager;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

/// Extract file_path from args (common pattern for LSP tools).
fn extract_file_path(args: &Value) -> Result<String> {
    Ok(
        args.get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?
            .to_string(),
    )
}

/// Extract a u64 field from args.
fn extract_u64(args: &Value, name: &str) -> Result<u32> {
    Ok(
        args.get(name)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing {name} parameter"))? as u32,
    )
}

pub async fn find_definition_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = extract_file_path(&args)?;
    let line = extract_u64(&args, "line")?;
    let character = extract_u64(&args, "character")?;

    // Auto-detect project from the file path.
    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .find_definition(&file_path, line, character)
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}

pub async fn find_references_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = extract_file_path(&args)?;
    let line = extract_u64(&args, "line")?;
    let character = extract_u64(&args, "character")?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .find_references(&file_path, line, character)
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}

pub async fn get_diagnostics_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = extract_file_path(&args)?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client.get_diagnostics(&file_path).await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}
