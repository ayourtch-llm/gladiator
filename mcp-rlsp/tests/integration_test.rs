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
async fn list_tools_returns_all_thirteen() {
    use mcp_rlsp::tools::get_tools;
    let tools = get_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert_eq!(names.len(), 13);
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
        // Project management tools
        "add_project",
        "switch_project",
        "list_projects",
        "get_active_project",
    ] {
        assert!(names.contains(expected), "missing tool: {}", expected);
    }
}

#[tokio::test]
async fn unknown_tool_returns_error() {
    use mcp_rlsp::analyzer::ProjectManager;
    let mut manager = ProjectManager::new();
    // Don't start the LSP — just verify execute_tool rejects unknown names.
    let result =
        mcp_rlsp::tools::execute_tool("nonexistent", serde_json::json!({}), &mut manager).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn find_definition_missing_params_returns_error() {
    use mcp_rlsp::analyzer::ProjectManager;
    let mut manager = ProjectManager::new();
    // No project registered, so this should fail when trying to get a client.
    let result =
        mcp_rlsp::tools::execute_tool("find_definition", serde_json::json!({}), &mut manager).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn list_projects_empty() {
    use mcp_rlsp::analyzer::ProjectManager;
    let mut manager = ProjectManager::new();
    // No projects registered — should return "No projects registered" text.
    let result =
        mcp_rlsp::tools::execute_tool("list_projects", serde_json::json!({}), &mut manager).await;
    assert!(result.is_ok());
    let r = result.unwrap();
    let text = r.content[0].get("text").unwrap().as_str().unwrap();
    assert!(text.contains("No projects registered"), "got: {text}");
}

#[tokio::test]
async fn add_and_list_project() {
    use mcp_rlsp::analyzer::ProjectManager;
    let mut manager = ProjectManager::new();
    // Add the current directory as a project.
    let cwd = std::env::current_dir().unwrap();
    let args = serde_json::json!({"project_path": cwd.display().to_string()});
    let result =
        mcp_rlsp::tools::execute_tool("add_project", args, &mut manager).await;
    assert!(result.is_ok());
    let r = result.unwrap();
    let text = r.content[0].get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Registered project"), "got: {text}");

    // List should show one active project.
    let list_result =
        mcp_rlsp::tools::execute_tool("list_projects", serde_json::json!({}), &mut manager).await;
    assert!(list_result.is_ok());
    let r = list_result.unwrap();
    let text = r.content[0].get("text").unwrap().as_str().unwrap();
    assert!(text.contains("ACTIVE"), "got: {text}");

    // get_active_project should return the path.
    let active_result =
        mcp_rlsp::tools::execute_tool("get_active_project", serde_json::json!({}), &mut manager).await;
    assert!(active_result.is_ok());
    let r = active_result.unwrap();
    let text = r.content[0].get("text").unwrap().as_str().unwrap();
    assert!(text.contains("Active project:"), "got: {text}");
}

#[tokio::test]
async fn switch_project_not_registered_fails() {
    use mcp_rlsp::analyzer::ProjectManager;
    let mut manager = ProjectManager::new();
    // Switch to a non-registered path — should fail.
    let args = serde_json::json!({"project_path": "/nonexistent/path"});
    let result =
        mcp_rlsp::tools::execute_tool("switch_project", args, &mut manager).await;
    assert!(result.is_err());
}
