use crate::analyzer::ProjectManager;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

fn get_str(args: &Value, name: &str) -> Result<String> {
    Ok(
        args.get(name)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing {name} parameter"))?
            .to_string(),
    )
}

fn get_u32(args: &Value, name: &str) -> Result<u32> {
    Ok(
        args.get(name)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing {name} parameter"))? as u32,
    )
}

pub async fn rename_symbol_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = get_str(&args, "file_path")?;
    let line = get_u32(&args, "line")?;
    let character = get_u32(&args, "character")?;
    let new_name = get_str(&args, "new_name")?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .rename_symbol(&file_path, line, character, &new_name)
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}

pub async fn extract_function_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = get_str(&args, "file_path")?;
    let start_line = get_u32(&args, "start_line")?;
    let start_character = get_u32(&args, "start_character")?;
    let end_line = get_u32(&args, "end_line")?;
    let end_character = get_u32(&args, "end_character")?;
    let function_name = get_str(&args, "function_name")?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .extract_function(
            &file_path,
            start_line,
            start_character,
            end_line,
            end_character,
            &function_name,
        )
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}

pub async fn inline_function_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = get_str(&args, "file_path")?;
    let line = get_u32(&args, "line")?;
    let character = get_u32(&args, "character")?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client
        .inline_function(&file_path, line, character)
        .await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}

pub async fn organize_imports_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let file_path = get_str(&args, "file_path")?;

    let client = manager.get_client_for_file(Some(file_path.as_str())).await?;
    let result = client.organize_imports(&file_path).await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}
