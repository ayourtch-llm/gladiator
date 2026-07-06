use crate::analyzer::ProjectManager;
use crate::tools::types::ToolResult;
use anyhow::Result;
use serde_json::{Value, json};

pub async fn workspace_symbols_impl(
    args: Value,
    manager: &mut ProjectManager,
) -> Result<ToolResult> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing query parameter"))?
        .to_string();

    // workspace_symbols doesn't take a file_path, so use the active project.
    let client = manager.get_client_for_file(None).await?;
    let result = client.workspace_symbols(&query).await?;

    Ok(ToolResult {
        content: vec![json!({"type": "text", "text": result})
            .as_object().unwrap().clone()],
    })
}
