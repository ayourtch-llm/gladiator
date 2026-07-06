use anyhow::Result;
use rmcp::{
    model::*,
    service::RequestContext,
    RoleServer,
    ServerHandler,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::analyzer::ProjectManager;
use crate::tools::{execute_tool, get_tools, ToolResult};

#[derive(Clone)]
pub struct RustMcpServer {
    manager: Arc<Mutex<ProjectManager>>,
}

impl Default for RustMcpServer {
    fn default() -> Self { Self::new() }
}

fn dbg_log(msg: &str) {
    eprintln!("[mcp-rlsp debug] {msg}");
}

impl RustMcpServer {
    pub fn new() -> Self {
        // Auto-register the CWD project on startup so existing single-project
        // usage continues to work without explicit add_project calls.
        let mut manager = ProjectManager::new();
        if let Ok(cwd) = std::env::current_dir() {
            let cwd_str = cwd.display().to_string();
            dbg_log(&format!("RustMcpServer::new: auto-registering CWD project {cwd_str}"));
            // add_project canonicalizes the path, so pass the directory.
            if !manager.add_project(&cwd_str).is_ok() {
                eprintln!("[mcp-rlsp debug] failed to auto-register CWD");
            }
        }
        Self {
            manager: Arc::new(Mutex::new(manager)),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        // RA starts lazily on first tool call via get_active_client_mut.
        // No eager startup — indexing takes 200+ seconds and would block the MCP handshake.
        Ok(())
    }

    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<ToolResult> {
        let mut manager = self.manager.lock().await;
        execute_tool(name, args, &mut manager).await
    }
}

#[async_trait::async_trait]
impl ServerHandler for RustMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation::from_build_env(),
            instructions: Some("Rust MCP Server providing rust-analyzer integration for idiomatic Rust development tools. Provides code analysis, refactoring, and project management capabilities.".to_string()),
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send {
        let tools = get_tools();
        let tool_list: Vec<Tool> = tools
            .into_iter()
            .map(|t| {
                Tool::new(
                    t.name.to_string(),
                    t.description.to_string(),
                    t.input_schema.as_ref().clone(),
                )
            })
            .collect();

        async move {
            Ok(ListToolsResult {
                tools: tool_list,
                next_cursor: None,
                meta: None,
            })
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send {
        let name = request.name.as_ref().to_string();
        let args = request.arguments.unwrap_or_default();
        let manager = self.manager.clone();

        async move {
            let mut mgr = manager.lock().await;
            match execute_tool(&name, Value::Object(args), &mut mgr).await {
                Ok(result) => {
                    let content: Vec<Content> = result
                        .content
                        .into_iter()
                        .filter_map(|m| {
                            m.get("text")
                                .and_then(|t| t.as_str())
                                .map(Content::text)
                        })
                        .collect();

                    Ok(CallToolResult {
                        content,
                        is_error: Some(false),
                        meta: None,
                        structured_content: None,
                    })
                }
                Err(e) => Ok(CallToolResult {
                    content: vec![Content::text(format!("Error: {e}"))],
                    is_error: Some(true),
                    meta: None,
                    structured_content: None,
                }),
            }
        }
    }
}
