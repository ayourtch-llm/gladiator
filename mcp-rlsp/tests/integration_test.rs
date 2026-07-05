//! Integration tests for mcp-rlsp server.

use rmcp::ServerHandler;
use mcp_rlsp::server::RustMcpServer;

#[test]
fn server_info_has_tools_capability() {
    let server = RustMcpServer::new();
    let info = ServerHandler::get_info(&server);
    assert!(info.capabilities.tools.is_some());
    assert!(info.instructions.unwrap().contains("rust-analyzer"));
}

#[tokio::test]
async fn list_tools_returns_all_nine() {
    use mcp_rlsp::tools::get_tools;
    let tools = get_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert_eq!(names.len(), 9);
    for expected in &[
        "find_definition",
        "find_references",
        "get_diagnostics",
        "workspace_symbols",
        "rename_symbol",
        "extract_function",
        "inline_function",
        "organize_imports",
        "get_type_hierarchy",
    ] {
        assert!(names.contains(expected), "missing tool: {}", expected);
    }
}

#[tokio::test]
async fn unknown_tool_returns_error() {
    use mcp_rlsp::analyzer::RustAnalyzerClient;
    let mut client = RustAnalyzerClient::new();
    // Don't start the LSP — just verify execute_tool rejects unknown names.
    let result =
        mcp_rlsp::tools::execute_tool("nonexistent", serde_json::json!({}), &mut client).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn find_definition_missing_params_returns_error() {
    use mcp_rlsp::analyzer::RustAnalyzerClient;
    let mut client = RustAnalyzerClient::new();
    let result =
        mcp_rlsp::tools::execute_tool("find_definition", serde_json::json!({}), &mut client).await;
    assert!(result.is_err());
}
