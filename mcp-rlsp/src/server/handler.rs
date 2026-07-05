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

use crate::analyzer::RustAnalyzerClient;
use crate::tools::{execute_tool, get_tools, ToolResult};

#[derive(Clone)]
pub struct RustMcpServer {
    analyzer: Arc<Mutex<RustAnalyzerClient>>,
}

impl Default for RustMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RustMcpServer {
    pub fn new() -> Self {
        Self {
            analyzer: Arc::new(Mutex::new(RustAnalyzerClient::new())),
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        let mut analyzer = self.analyzer.lock().await;
        analyzer.start().await
    }

    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<ToolResult> {
        let mut analyzer = self.analyzer.lock().await;
        execute_tool(name, args, &mut analyzer).await
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
        let analyzer = self.analyzer.clone();

        async move {
            let mut client = analyzer.lock().await;
            match execute_tool(&name, Value::Object(args), &mut client).await {
                Ok(result) => {
                    let content: Vec<Content> = result
                        .content
                        .into_iter()
                        .filter_map(|m| {
                            m.get("text")
                                .and_then(|t| t.as_str())
                                .map(|t| Content::text(t))
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
                    is_error: Some(false),
                    meta: None,
                    structured_content: None,
                }),
            }
        }
    }
}
