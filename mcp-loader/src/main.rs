//! mcp-loader — a dynamic MCP loader.
//!
//! This binary is BOTH an MCP *server* (the bot connects to it over stdio) and an
//! MCP *client* (it spawns + connects to other MCP servers on demand). It lets the
//! bot load/unload other MCP servers AT RUNTIME and call their tools — without the
//! bot needing any restart or list-changed plumbing. The bot connects to this one
//! loader once; everything else is dynamic.
//!
//! Tools exposed to the bot:
//!   - mcp_load(name, command)         spawn + connect a sub-server, return its tools
//!   - mcp_unload(name)                disconnect + terminate a sub-server
//!   - mcp_list()                      list loaded servers + their tools
//!   - mcp_call(server, tool, args)    forward a tool call to a loaded sub-server

use anyhow::Result;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    schemars,
    service::RoleClient,
    tool, tool_handler, tool_router,
    transport::{stdio, TokioChildProcess},
    ClientHandler, Peer, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

// ── Helpers ────────────────────────────────────────────────────────

fn tool_error(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}
fn tool_ok_text(s: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s)])
}

// Minimal client handler — we never service server→client requests.
#[derive(Clone)]
struct ClientH;
impl ClientHandler for ClientH {}

// One loaded sub-server connection.
struct Loaded {
    peer: Peer<RoleClient>,
    // Aborting this handle drops the RunningService → closes stdio → child exits.
    handle: tokio::task::JoinHandle<()>,
    tools: Vec<String>,
}

#[derive(Clone)]
struct LoaderServer {
    tool_router: ToolRouter<Self>,
    servers: Arc<Mutex<HashMap<String, Loaded>>>,
}

impl LoaderServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            servers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// ── Parameter structs ──────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LoadParams {
    /// Short name to refer to this server later (e.g. "pdf").
    name: String,
    /// argv to launch the MCP server, e.g. ["/abs/path/to/mcp-pdf-server"].
    command: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UnloadParams {
    /// Name used in mcp_load.
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CallParams {
    /// Name of a previously-loaded server.
    server: String,
    /// Tool name on that server.
    tool: String,
    /// JSON object of arguments for the tool (or omit for none).
    /// NOTE: schema overridden to a typed object — a bare serde_json::Value
    /// generates an untyped {default:null} schema that strict OpenAI backends
    /// (llama.cpp / Qwen) reject with "Unrecognized schema". A map renders as
    /// {"type":"object"}, which they accept. Field stays Value (usage unchanged).
    #[serde(default)]
    #[schemars(with = "std::collections::HashMap<String, serde_json::Value>")]
    arguments: serde_json::Value,
}

// ── Tools ──────────────────────────────────────────────────────────

#[tool_router]
impl LoaderServer {
    #[tool(
        description = "Load (spawn + connect to) an MCP server at runtime under a name. `command` is the argv array, e.g. [\"/abs/path/to/server-binary\"]. Returns the list of tool names the server exposes; call them with mcp_call."
    )]
    async fn mcp_load(
        &self,
        Parameters(p): Parameters<LoadParams>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        if p.command.is_empty() {
            return Ok(tool_error("command must have at least one element"));
        }
        let mut servers = self.servers.lock().await;
        if servers.contains_key(&p.name) {
            return Ok(tool_error(format!(
                "server '{}' already loaded; mcp_unload it first",
                p.name
            )));
        }

        let mut cmd = tokio::process::Command::new(&p.command[0]);
        cmd.args(&p.command[1..]);
        let (transport, _stderr) = match TokioChildProcess::builder(cmd).spawn() {
            Ok(v) => v,
            Err(e) => return Ok(tool_error(format!("spawn failed: {}", e))),
        };

        let service = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            rmcp::serve_client(ClientH, transport),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Ok(tool_error(format!("connect failed: {}", e))),
            Err(_) => return Ok(tool_error("timed out connecting to server (30s)")),
        };

        let peer = service.peer().clone();
        let tools: Vec<String> = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            peer.list_all_tools(),
        )
        .await
        {
            Ok(Ok(ts)) => ts.into_iter().map(|t| t.name.to_string()).collect(),
            Ok(Err(e)) => return Ok(tool_error(format!("list_tools failed: {}", e))),
            Err(_) => return Ok(tool_error("timed out listing tools (30s)")),
        };

        let handle = tokio::spawn(async move {
            let _ = service.waiting().await;
        });
        let summary = format!(
            "loaded '{}' with {} tools: {}",
            p.name,
            tools.len(),
            tools.join(", ")
        );
        servers.insert(p.name.clone(), Loaded { peer, handle, tools });
        Ok(tool_ok_text(summary))
    }

    #[tool(description = "Unload (disconnect + terminate) a previously-loaded MCP server by name.")]
    async fn mcp_unload(
        &self,
        Parameters(p): Parameters<UnloadParams>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let mut servers = self.servers.lock().await;
        match servers.remove(&p.name) {
            Some(loaded) => {
                loaded.handle.abort();
                Ok(tool_ok_text(format!("unloaded '{}'", p.name)))
            }
            None => Ok(tool_error(format!("server '{}' not loaded", p.name))),
        }
    }

    #[tool(description = "List currently-loaded MCP servers and the tools each one exposes.")]
    async fn mcp_list(&self) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let servers = self.servers.lock().await;
        if servers.is_empty() {
            return Ok(tool_ok_text("(no servers loaded)".to_string()));
        }
        let mut out = String::new();
        for (name, l) in servers.iter() {
            out.push_str(&format!("{}: {}\n", name, l.tools.join(", ")));
        }
        Ok(tool_ok_text(out))
    }

    #[tool(
        description = "Call a tool on a loaded MCP server. `server` = name used in mcp_load, `tool` = tool name, `arguments` = JSON object of args. Returns the tool's text output."
    )]
    async fn mcp_call(
        &self,
        Parameters(p): Parameters<CallParams>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let servers = self.servers.lock().await;
        let loaded = match servers.get(&p.server) {
            Some(l) => l,
            None => {
                return Ok(tool_error(format!(
                    "server '{}' not loaded (use mcp_load first)",
                    p.server
                )))
            }
        };

        let arguments = match p.arguments {
            serde_json::Value::Object(m) => Some(m),
            serde_json::Value::Null => None,
            other => {
                return Ok(tool_error(format!(
                    "arguments must be a JSON object, got: {}",
                    other
                )))
            }
        };

        let result = match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            loaded.peer.call_tool(CallToolRequestParams {
                name: p.tool.clone().into(),
                arguments,
                meta: None,
                task: None,
            }),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Ok(tool_error(format!("call_tool failed: {}", e))),
            Err(_) => return Ok(tool_error("tool call timed out (120s)")),
        };

        // Flatten the sub-tool's text content.
        let mut text = String::new();
        for c in result.content.iter() {
            if let Some(t) = c.as_text() {
                text.push_str(&t.text);
                text.push('\n');
            }
        }
        if result.is_error.unwrap_or(false) {
            Ok(tool_error(format!("sub-tool reported error: {}", text)))
        } else {
            Ok(tool_ok_text(text))
        }
    }
}

// ── Handler ────────────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for LoaderServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Dynamic MCP loader: mcp_load / mcp_unload / mcp_list / mcp_call to \
                 manage and call other MCP servers at runtime."
                    .to_string(),
            ),
        }
    }
}

// ── Main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Logs MUST go to stderr — stdout is the MCP protocol channel.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting mcp-loader");
    let server = LoaderServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
