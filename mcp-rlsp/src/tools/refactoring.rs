use crate::analyzer::RustAnalyzerClient;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

pub async fn rename_symbol_impl(
    args: Value,
    analyzer: &mut RustAnalyzerClient,
) -> Result<ToolResult> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?;
    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("Missing line parameter"))?;
    let character = args
        .get("character")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("Missing character parameter"))?;
    let new_name = args
        .get("new_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing new_name parameter"))?;

    // Implementation will use rust-analyzer LSP to rename symbol
    let result = analyzer
        .rename_symbol(file_path, line as u32, character as u32, new_name)
        .await?;

    Ok(ToolResult {
        content: vec![
            json!({
                "type": "text",
                "text": result
            })
            .as_object()
            .unwrap()
            .clone(),
        ],
    })
}

pub async fn extract_function_impl(
    args: Value,
    _analyzer: &mut RustAnalyzerClient,
) -> Result<ToolResult> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?;
    // textDocument/codeAction returns NULL when using rust-project.json mode,
    // which we need for workspace manifest support. These refactoring tools
    // require cargo-based project discovery that conflicts with our setup.
    Ok(ToolResult {
        content: vec![
            json!({
                "type": "text",
                "text": format!("extract_function is unavailable in rust-project.json mode (codeAction not supported). Use the extract_function tool from another LSP client or edit manually. File: {}", file_path)
            })
            .as_object()
            .unwrap()
            .clone(),
        ],
    })
}

pub async fn inline_function_impl(
    args: Value,
    _analyzer: &mut RustAnalyzerClient,
) -> Result<ToolResult> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?;
    Ok(ToolResult {
        content: vec![
            json!({
                "type": "text",
                "text": format!("inline_function is unavailable in rust-project.json mode (codeAction not supported). Use the inline_function tool from another LSP client or edit manually. File: {}", file_path)
            })
            .as_object()
            .unwrap()
            .clone(),
        ],
    })
}

pub async fn organize_imports_impl(
    args: Value,
    _analyzer: &mut RustAnalyzerClient,
) -> Result<ToolResult> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing file_path parameter"))?;
    Ok(ToolResult {
        content: vec![
            json!({
                "type": "text",
                "text": format!("organize_imports is unavailable in rust-project.json mode (codeAction not supported). Sort use statements manually or use an external tool. File: {}", file_path)
            })
            .as_object()
            .unwrap()
            .clone(),
        ],
    })
}
