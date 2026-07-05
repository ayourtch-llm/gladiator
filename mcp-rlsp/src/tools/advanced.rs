use crate::analyzer::RustAnalyzerClient;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

pub async fn get_type_hierarchy_impl(
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

    let result = analyzer
        .get_type_hierarchy(file_path, line as u32, character as u32)
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
