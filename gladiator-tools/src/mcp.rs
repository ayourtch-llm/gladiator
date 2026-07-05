use crate::tool::Tool;
use gladiator_core::McpServerConfig;
use rmcp::{
    model::{CallToolRequestParams, RawContent, Tool as RmcpTool},
    service::RunningService,
    transport::TokioChildProcess,
    ClientHandler, RoleClient,
};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// A single MCP tool wrapped as a gladiator Tool.
pub struct McpTool {
    tool_name: String,
    tool_description: String,
    tool_parameters: serde_json::Value,
    peer: Arc<Mutex<rmcp::Peer<RoleClient>>>,
}

impl McpTool {
    pub fn new(tool: &RmcpTool, peer: Arc<Mutex<rmcp::Peer<RoleClient>>>) -> Self {
        Self {
            tool_name: tool.name.as_ref().to_string(),
            tool_description: tool
                .description
                .as_ref()
                .map(|s| s.as_ref().to_string())
                .unwrap_or_default(),
            tool_parameters: tool_input_schema_to_json(&tool.input_schema),
            peer,
        }
    }
}

fn normalize_schema(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Bool(true) => serde_json::json!({}),
        serde_json::Value::Object(map) => {
            let normalized: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), normalize_schema(v)))
                .collect();
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(arr) => {
            let normalized: Vec<serde_json::Value> =
                arr.iter().map(normalize_schema).collect();
            serde_json::Value::Array(normalized)
        }
        other => other.clone(),
    }
}

fn tool_input_schema_to_json(
    schema: &Arc<serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Value {
    normalize_schema(&serde_json::Value::Object(schema.as_ref().clone()))
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters(&self) -> serde_json::Value {
        self.tool_parameters.clone()
    }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String> {
        debug!("[mcp-tool] executing {} with args: {:?}", self.tool_name, arguments);
        let peer = self.peer.lock().await;
        let args_map: serde_json::Map<String, serde_json::Value> = if let serde_json::Value::Object(m) = arguments {
            m.clone()
        } else {
            serde_json::Map::new()
        };
        let call_params = CallToolRequestParams {
            name: self.tool_name.clone().into(),
            arguments: Some(args_map),
            meta: None,
            task: None,
        };

        debug!("[mcp-tool] calling peer.call_tool with 120s timeout...");
        debug!("[mcp-tool] peer info: {:?}", peer.peer_info());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            peer.call_tool(call_params)
        ).await;
        debug!("[mcp-tool] call_tool result: {:?}", result.is_ok());
        match result {
            Ok(Ok(result)) => {
                let content = result
                    .content
                    .iter()
                    .filter_map(|c| match &c.raw {
                        RawContent::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let is_error = result.is_error.unwrap_or(false);
                if is_error {
                    Err(format!("MCP error: {}", content))
                } else {
                    Ok(content)
                }
            }
            Ok(Err(e)) => Err(format!("MCP call failed: {}", e)),
            Err(_) => Err("MCP tool call timed out after 10 seconds".to_string()),
        }
    }
}

/// Empty handler for MCP client messages.
#[derive(Clone)]
pub struct McpClientHandler;
impl ClientHandler for McpClientHandler {}

/// Lightweight handle returned after spawning an MCP server.
/// Owns the service loop JoinHandle so it can shut down the child process.
pub struct McpServerHandle {
    prefix: String,
    peer: Arc<Mutex<rmcp::Peer<RoleClient>>>,
    tools: Vec<RmcpTool>,
    config: McpServerConfig,
    service_handle: Option<tokio::task::JoinHandle<()>>,
}

impl McpServerHandle {
    /// Get tool actors for tools that should be exposed to the LLM.
    pub fn tool_actors(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .iter()
            .filter(|t| {
                let should_include = |name: &str| -> bool {
                    if self.config.default {
                        return true;
                    }
                    if !self.config.expose.is_empty() {
                        return self.config.expose.contains(&name.to_string());
                    }
                    false
                };
                should_include(t.name.as_ref())
            })
            .map(|t| {
                Arc::new(McpTool::new(t, self.peer.clone())) as Arc<dyn Tool>
            })
            .collect()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name.as_ref().to_string()).collect()
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Shut down the MCP server child process by aborting the service loop.
    /// When the service loop is dropped, the transport closes and the child process exits.
    pub fn shutdown(mut self) {
        if let Some(handle) = self.service_handle.take() {
            handle.abort();
        }
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.service_handle.take() {
            handle.abort();
        }
    }
}

/// Runner to spawn an MCP server process and discover its tools.
pub struct McpServerRunner {
    prefix: String,
    config: McpServerConfig,
}

impl McpServerRunner {
    pub fn new(prefix: String, config: McpServerConfig) -> Self {
        Self { prefix, config }
    }

    pub async fn spawn(
        &self,
    ) -> Result<McpServerHandle, Box<dyn std::error::Error + Send + Sync>> {
        if self.config.command.is_empty() {
            return Err("MCP server command is empty".into());
        }

        let cmd = {
            let mut c = tokio::process::Command::new(&self.config.command[0]);
            c.args(&self.config.command[1..]);
            c
        };
        let (transport, stderr) = TokioChildProcess::builder(cmd)
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(stderr_handle) = stderr {
            let prefix = self.prefix.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr_handle);
                let mut lines_stream = reader.lines();
                while let Ok(Some(text)) = lines_stream.next_line().await {
                    if text.contains("ERROR") || text.contains("error") {
                        warn!("[mcp:{}:stderr] {}", prefix, text);
                    } else {
                        debug!("[mcp:{}:stderr] {}", prefix, text);
                    }
                }
            });
        }

        let service = McpClientHandler;
        let running_service: RunningService<RoleClient, McpClientHandler> =
            rmcp::serve_client(service, transport).await?;
        let peer = Arc::new(Mutex::new(running_service.peer().clone()));

        info!("MCP server '{}' started, discovering tools...", self.prefix);

        let tools: Vec<RmcpTool> = peer.lock().await.list_all_tools().await?;

        info!(
            "MCP server '{}' exposed {} tools",
            self.prefix,
            tools.len()
        );

        // Keep the service loop alive in background
        let service_handle = tokio::spawn(async move {
            debug!("[mcp] waiting for service loop to complete...");
            let result = running_service.waiting().await;
            debug!("[mcp] service loop completed: {:?}", result.is_ok());
            let _ = result;
        });

        Ok(McpServerHandle {
            prefix: self.prefix.clone(),
            peer,
            tools,
            config: self.config.clone(),
            service_handle: Some(service_handle),
        })
    }
}
