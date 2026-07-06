//! Integration test that spawns rust-analyzer and verifies LSP queries
//! on real workspace files.
use mcp_rlsp::analyzer::*;
use std::env;
use std::time::Duration;

fn find_crate_root() -> String {
    let cwd = env::current_dir().unwrap();
    if cwd.join("mcp-rlsp/src/main.rs").exists() {
        return cwd.join("mcp-rlsp").display().to_string();
    }
    if cwd.join("src/main.rs").exists()
        && cwd.file_name().map(|n| n == "mcp-rlsp").unwrap_or(false)
    {
        return cwd.display().to_string();
    }
    cwd.join("mcp-rlsp").display().to_string()
}

/// Spawns rust-analyzer and waits 10s for indexing — slow, run separately.
#[tokio::test]
#[ignore]
async fn lsp_find_definition_with_wait() {
    let mut client = RustAnalyzerClient::new();
    client.start().await.expect("RA should start");

    let path = find_crate_root() + "/src/main.rs";
    eprintln!("find_definition on: {}", path);

    // did_open first, then wait 10s for indexing to settle
    client.did_open(&path).await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Try find_definition at line 5 char 12 (inside "RustMcpServer" on that line)
    let result = client.find_definition(&path, 5, 14).await;
    eprintln!("find_definition -> {:?}", result);
}

/// Spawns rust-analyzer and waits 10s for indexing — slow, run separately.
#[tokio::test]
#[ignore]
async fn lsp_organize_imports_with_wait() {
    let mut client = RustAnalyzerClient::new();
    client.start().await.expect("RA should start");

    // Write a file with out-of-order imports to the workspace.
    let crate_root = find_crate_root();
    let test_file = format!("{}/src/test_lsp_action.rs", crate_root);
    std::fs::write(
        &test_file,
        "use serde_json::json;\n\
         use anyhow::Result;\n\n\
         pub fn make() -> Result<serde_json::Value> {\n\
             Ok(json!({}))\n\
         }\n",
    )
    .unwrap();
    eprintln!("organize_imports on: {}", test_file);

    client.did_open(&test_file).await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;

    let result = client.organize_imports(&test_file).await;
    eprintln!("organize_imports -> {:?}", result);
    let _ = std::fs::remove_file(&test_file);
}

/// Spawns rust-analyzer and waits 30s for indexing — very slow, run separately.
#[tokio::test]
#[ignore]
async fn lsp_workspace_symbols_long_wait() {
    eprintln!("CWD at test time: {:?}", std::env::current_dir());
    let mut client = RustAnalyzerClient::new();
    client.start().await.expect("RA should start");

    // Wait 30s for RA to fully index the project.
    eprintln!("Waiting 30s for indexing...");
    tokio::time::sleep(Duration::from_secs(30)).await;

    let result = client.workspace_symbols("").await;
    eprintln!("workspace_symbols '' (all) -> {:?}", result);

    // Also try a specific query
    let result2 = client.workspace_symbols("RustMcpServer").await;
    eprintln!("workspace_symbols 'RustMcpServer' -> {:?}", result2);
}
