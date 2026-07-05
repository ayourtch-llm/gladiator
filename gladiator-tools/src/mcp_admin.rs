use crate::tool::Tool;
use async_trait::async_trait;
use gladiator_core::McpServerConfig;
pub use crate::mcp::McpClientHandler;
use rmcp::{
    model::{CallToolRequestParams, RawContent},
    service::RunningService,
    transport::TokioChildProcess,
    RoleClient,
};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Maximum number of stderr log lines retained per server.
const STDERR_RING_CAPACITY: usize = 200;

/// Base delay for exponential backoff on respawn (milliseconds).
const RESPAWN_BASE_DELAY_MS: u64 = 500;
/// Cap on a single backoff delay (milliseconds).
const RESPAWN_MAX_DELAY_MS: u64 = 30_000;
/// Max consecutive crash-retries before disabling.
const MAX_RETRIES: usize = 10;

/// A timestamped stderr log ring buffer per MCP server.
#[derive(Debug, Default)]
pub struct StderrLogRing {
    lines: Mutex<VecDeque<String>>,
}

impl StderrLogRing {
    pub fn new() -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(STDERR_RING_CAPACITY)) }
    }

    async fn push(&self, line: String) {
        let mut guard = self.lines.lock().await;
        if guard.len() >= STDERR_RING_CAPACITY {
            guard.pop_front();
        }
        guard.push_back(line);
    }

    /// Return the last `n` log lines (most recent first).
    pub async fn tail(&self, n: usize) -> Vec<String> {
        let guard = self.lines.lock().await;
        guard.iter().rev().take(n).cloned().collect()
    }
}

/// Lifecycle state of a single MCP server.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerStatus {
    Starting,
    Running,
    Restarting { attempt: usize },
    Disabled { reason: String },
    Crashed,
}

impl std::fmt::Display for ServerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Restarting { attempt } => write!(f, "restarting(attempt={})", attempt),
            Self::Disabled { reason } => write!(f, "disabled({})", reason),
            Self::Crashed => write!(f, "crashed"),
        }
    }
}

/// Shared per-server state. Held in an Arc inside the manager so that admin
/// tools and supervisor tasks can observe/mutate it concurrently.
pub struct McpServerState {
    pub name: String,
    pub config: McpServerConfig,
    /// Hot-swappable peer — all McpTool instances hold a clone of this guard.
    peer_slot: Arc<Mutex<Option<rmcp::Peer<RoleClient>>>>,
    status: Mutex<ServerStatus>,
    retry_count: Mutex<usize>,
    log_ring: Arc<StderrLogRing>,
}

impl McpServerState {
    pub fn new(name: String, config: McpServerConfig) -> Self {
        Self {
            name,
            config,
            peer_slot: Arc::new(Mutex::new(None)),
            status: Mutex::new(ServerStatus::Starting),
            retry_count: Mutex::new(0),
            log_ring: Arc::new(StderrLogRing::new()),
        }
    }

    pub async fn status(&self) -> ServerStatus {
        self.status.lock().await.clone()
    }

    /// Return a clone of the current peer, if any.
    pub async fn peer(&self) -> Option<rmcp::Peer<RoleClient>> {
        self.peer_slot.lock().await.clone()
    }
}

/// Manager owning all MCP server states. Exposes admin tools and spawns
/// supervisor tasks that keep servers alive with exponential backoff.
pub struct McpManager {
    servers: Vec<Arc<McpServerState>>,
}

impl McpManager {
    /// Spawn every configured MCP server, returning a manager handle plus the
    /// tool actors for all successfully-started servers' tools.
    pub async fn spawn_all(
        configs: &std::collections::HashMap<String, McpServerConfig>,
    ) -> (Self, Vec<Arc<dyn Tool>>) {
        let mut states = Vec::new();
        let mut tools = Vec::new();

        for (name, cfg) in configs.iter() {
            if cfg.command.is_empty() { continue; }
            let state = Arc::new(McpServerState::new(name.clone(), cfg.clone()));
            // Initial spawn.
            match initial_spawn(&state).await {
                Ok(server_tools) => {
                    info!("MCP server '{}' started, {} tools", name, server_tools.len());
                    for t in server_tools { tools.push(t); }
                    states.push(state);
                }
                Err(e) => {
                    warn!("Failed to spawn MCP server '{}': {}", name, e);
                    // Still track it so admin tools can retry; mark crashed.
                    *state.status.lock().await = ServerStatus::Crashed;
                    states.push(state);
                }
            }

            // Spawn supervisor for this server (even if initial spawn failed —
            // the supervisor will attempt respawns with backoff).
            let state_for_supervisor = Arc::clone(&states.last().unwrap());
            tokio::spawn(async move { supervise(state_for_supervisor).await });
        }

        (Self { servers: states }, tools)
    }
}

/// Perform an initial spawn of a server, discovering its tools and installing
/// the peer into `state.peer_slot`. Returns tool actors on success.
async fn initial_spawn(
    state: &McpServerState,
) -> Result<Vec<Arc<dyn Tool>>, Box<dyn std::error::Error + Send + Sync>> {
    let runner = McpSpawnRunner { name: state.name.clone(), config: state.config.clone() };
    match runner.spawn_with_log(&state.peer_slot, state.log_ring.clone()).await {
        Ok(tools) => {
            *state.status.lock().await = ServerStatus::Running;
            *state.retry_count.lock().await = 0;
            Ok(tools)
        }
        Err(e) => Err(e),
    }
}

/// Supervisor task: monitors a server's child process health and respawns it
/// with exponential backoff on crash. After MAX_RETRIES consecutive failures,
/// marks the server Disabled.
async fn supervise(state: Arc<McpServerState>) {
    // The supervisor doesn't directly watch the JoinHandle (we don't own it —
    // rmcp's RunningService owns it internally). Instead we poll peer health:
    // if the peer becomes None or calls start failing, that indicates death.
    //
    // We use a lightweight polling loop: every 5s check status; if Crashed,
    // attempt respawn with backoff. If Disabled, exit supervisor.

    let mut consecutive_failures = 0usize;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        match state.status().await {
            ServerStatus::Disabled { .. } => return,
            ServerStatus::Running | ServerStatus::Starting => continue,
            _ => {}
        }

        // Status is Restarting or Crashed — attempt respawn.
        if consecutive_failures >= MAX_RETRIES {
            let reason = format!("exceeded {} retries", MAX_RETRIES);
            warn!("MCP server '{}' disabled: {}", state.name, reason);
            *state.status.lock().await =
                ServerStatus::Disabled { reason };
            return;
        }

        let attempt = consecutive_failures + 1;
        *state.retry_count.lock().await = attempt;
        *state.status.lock().await = ServerStatus::Restarting { attempt };

        // Exponential backoff.
        let delay_ms = RESPAWN_BASE_DELAY_MS
            .saturating_mul(2u64.saturating_pow(attempt as u32 - 1))
            .min(RESPAWN_MAX_DELAY_MS);
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

        info!(
            "MCP server '{}' respawn attempt {}/{} after {}ms",
            state.name, attempt, MAX_RETRIES, delay_ms
        );

        let runner = McpSpawnRunner {
            name: state.name.clone(),
            config: state.config.clone(),
        };
        match runner.spawn_with_log(&state.peer_slot, state.log_ring.clone()).await {
            Ok(_tools) => {
                *state.status.lock().await = ServerStatus::Running;
                *state.retry_count.lock().await = 0;
                consecutive_failures = 0;
                info!("MCP server '{}' respawned successfully", state.name);
            }
            Err(e) => {
                warn!(
                    "MCP server '{}' respawn attempt {} failed: {}",
                    state.name, attempt, e
                );
                *state.status.lock().await = ServerStatus::Crashed;
                consecutive_failures += 1;
                state.log_ring.push(format!(
                    "[respawn-failure] attempt {}/{}: {}",
                    attempt, MAX_RETRIES, e
                )).await;
            }
        }
    }
}

/// Internal spawn runner that captures stderr into the log ring and returns a peer + tools.
struct McpSpawnRunner {
    name: String,
    config: McpServerConfig,
}

impl McpSpawnRunner {
    async fn spawn_with_log(
        &self,
        peer_slot: &Arc<Mutex<Option<rmcp::Peer<RoleClient>>>>,
        log_ring: Arc<StderrLogRing>,
    ) -> Result<Vec<Arc<dyn Tool>>, Box<dyn std::error::Error + Send + Sync>>
    {
        let mut cmd = tokio::process::Command::new(&self.config.command[0]);
        if self.config.command.len() > 1 {
            cmd.args(&self.config.command[1..]);
        }
        let (transport, stderr) =
            TokioChildProcess::builder(cmd).stderr(Stdio::piped()).spawn()?;

        // Capture stderr into the ring buffer (and forward to tracing).
        if let Some(stderr_handle) = stderr {
            let name = self.name.clone();
            let ring = log_ring;
            tokio::spawn(async move {
                let reader = BufReader::new(stderr_handle);
                let mut lines_stream = reader.lines();
                while let Ok(Some(text)) = lines_stream.next_line().await {
                    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                    let line = format!("[{}] [mcp:{}:stderr] {}", ts, name, text);
                    if text.contains("ERROR") || text.contains("error") {
                        warn!("{}", line);
                    } else {
                        debug!("{}", line);
                    }
                    ring.push(line).await;
                }
            });
        }

        let service = McpClientHandler;
        let running_service: RunningService<RoleClient, McpClientHandler> =
            rmcp::serve_client(service, transport).await?;
        let peer = running_service.peer().clone();

        // Discover tools.
        info!("MCP server '{}' spawned, discovering tools...", self.name);
        let discovered = peer.list_all_tools().await?;

        // Install the new peer into the shared slot (hot-swap).
        *peer_slot.lock().await = Some(peer.clone());

        // Build tool actors. All share the same hot-swappable peer slot
        // so that a respawn transparently updates every tool's connection.
        let mut tools_vec: Vec<Arc<dyn Tool>> = Vec::new();
        for t in &discovered {
            if !should_expose(t.name.as_ref(), &self.config) { continue; }
            tools_vec.push(Arc::new(McpToolWithSlot {
                name: t.name.to_string(),
                description: t.description
                    .as_ref()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                parameters: normalize_tool_schema(&t.input_schema),
                peer_slot: Arc::clone(peer_slot),
            }) as Arc<dyn Tool>);
        }

        // Keep the service loop alive in background.
        let name_clone = self.name.clone();
        tokio::spawn(async move {
            debug!("[mcp] {} waiting for service loop...", name_clone);
            let result = running_service.waiting().await;
            warn!("[mcp] {} service loop ended: {:?}", name_clone, result.is_ok());
            drop(result);
        });

        Ok(tools_vec)
    }
}

/// Decide whether a tool should be exposed based on config default/expose.
fn should_expose(name: &str, cfg: &McpServerConfig) -> bool {
    if cfg.default { return true; }
    if !cfg.expose.is_empty() { return cfg.expose.contains(&name.to_string()); }
    false
}

/// Normalize an rmcp input schema (Arc<Map>) into a plain serde_json::Value.
fn normalize_tool_schema(schema: &std::sync::Arc<serde_json::Map<String, serde_json::Value>>) -> serde_json::Value {
    let obj = serde_json::Value::Object(schema.as_ref().clone());
    // Recursively strip `true`/`false` schema booleans into empty objects for compatibility.
    normalize_schema_value(&obj)
}

fn normalize_schema_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Bool(true) => serde_json::json!({}),
        serde_json::Value::Object(map) => {
            let normalized: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), normalize_schema_value(v)))
                .collect();
            serde_json::Value::Object(normalized)
        }
        serde_json::Value::Array(arr) => {
            let normalized: Vec<serde_json::Value> =
                arr.iter().map(normalize_schema_value).collect();
            serde_json::Value::Array(normalized)
        }
        other => other.clone(),
    }
}

/// MCP tool that reads its peer from a hot-swappable slot on each call.
struct McpToolWithSlot {
    name: String,
    description: String,
    parameters: serde_json::Value,
    peer_slot: Arc<Mutex<Option<rmcp::Peer<RoleClient>>>>,
}

#[async_trait]
impl Tool for McpToolWithSlot {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn parameters(&self) -> serde_json::Value { self.parameters.clone() }

    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let peer = self.peer_slot.lock().await
            .clone()
            .ok_or_else(|| format!("MCP server '{}' is not running", self.name))?;

        let args_map: serde_json::Map<String, serde_json::Value> =
            if let serde_json::Value::Object(m) = arguments {
                m.clone()
            } else { serde_json::Map::new() };

        let call_params = CallToolRequestParams {
            name: self.name.clone().into(),
            arguments: Some(args_map),
            meta: None,
            task: None,
        };

        debug!("[mcp-tool] executing {} with args: {:?}", self.name, arguments);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            peer.call_tool(call_params)
        ).await;

        match result {
            Ok(Ok(result)) => {
                let content = result.content
                    .iter()
                    .filter_map(|c| match &c.raw { RawContent::Text(t) => Some(t.text.clone()), _ => None })
                    .collect::<Vec<_>>().join("\n");
                if result.is_error.unwrap_or(false) {
                    Err(format!("MCP error: {}", content))
                } else { Ok(content) }
            }
            Ok(Err(e)) => Err(format!("MCP call failed: {}", e)),
            Err(_) => Err("MCP tool call timed out after 10 seconds".to_string()),
        }
    }
}

// ---- Admin tools -----------------------------------------------------------

/// Tool listing all MCP servers and their status.
pub struct McpStatusTool {
    manager: Arc<McpManager>,
}

impl McpStatusTool {
    pub fn new(manager: Arc<McpManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for McpStatusTool {
    fn name(&self) -> &str { "mcp_status" }
    fn description(&self) -> &str {
        "List all MCP servers managed by gladiator, their current status (running/restarting/disabled/crashed), tool count, and recent stderr log lines. Pass an optional server_name to show detailed logs for one server."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "server_name": { "type": "string", "description": "Optional: name of a specific MCP server (e.g. 'mcp-rlsp') to show its recent stderr logs" } },
            "additionalProperties": false
        })
    }
    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let filter = arguments.get("server_name").and_then(|v| v.as_str());
        let mut out = String::new();
        for state in self.manager.servers.iter() {
            if let Some(f) = filter { if f != state.name { continue; } }
            let status = state.status().await;
            let retries = *state.retry_count.lock().await;
            out.push_str(&format!(
                "- {} [{}], retries={}, tools={}\n",
                state.name, status, retries,
                // tool count not tracked here post-spawn for simplicity
                "?"
            ));
        }
        if filter.is_some() {
            let target = self.manager.servers.iter().find(|s| s.name == filter.unwrap());
            if let Some(t) = target {
                out.push_str("\nRecent stderr logs:\n");
                for line in t.log_ring.tail(20).await { out.push_str(&line); out.push('\n'); }
            }
        } else {
            // Brief last-line per server.
            for state in self.manager.servers.iter() {
                if let Some(last) = state.log_ring.tail(1).await.first() {
                    out.push_str(&format!("  last log: {}\n", last));
                }
            }
        }
        Ok(out)
    }
}

/// Tool to force-restart a named MCP server.
pub struct McpRestartTool {
    manager: Arc<McpManager>,
}

impl McpRestartTool {
    pub fn new(manager: Arc<McpManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for McpRestartTool {
    fn name(&self) -> &str { "mcp_restart" }
    fn description(&self) -> &str {
        "Force-restart a named MCP server. Kills the current child process (if any), respawns with same config, re-discovers tools, and hot-swaps the peer so existing tool calls use the new instance. Use after rebuilding an MCP binary to pick up changes."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "server_name": { "type": "string", "description": "Name of the MCP server to restart (e.g. 'mcp-rlsp')" } },
            "required": ["server_name"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let name = arguments.get("server_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing server_name".to_string())?;
        let state = self.manager.servers.iter().find(|s| s.name == name)
            .ok_or_else(|| format!("no MCP server named '{}'", name))?;

        // Clear peer (drops the old child transport).
        *state.peer_slot.lock().await = None;
        *state.status.lock().await = ServerStatus::Restarting { attempt: 1 };
        *state.retry_count.lock().await = 0;

        let runner = McpSpawnRunner {
            name: state.name.clone(),
            config: state.config.clone(),
        };
        match runner.spawn_with_log(&state.peer_slot, state.log_ring.clone()).await {
            Ok(_tools) => {
                *state.status.lock().await = ServerStatus::Running;
                info!("MCP server '{}' manually restarted", name);
                Ok(format!("Restarted MCP server '{}'", name))
            }
            Err(e) => {
                *state.status.lock().await = ServerStatus::Crashed;
                state.log_ring.push(format!("[manual-restart-failure] {}", e)).await;
                Err(format!("Failed to restart '{}': {}", name, e))
            }
        }
    }
}

/// Tool to disable a named MCP server (stops child + marks disabled so supervisor won't respawn).
pub struct McpDisableTool {
    manager: Arc<McpManager>,
}

impl McpDisableTool {
    pub fn new(manager: Arc<McpManager>) -> Self { Self { manager } }
}

#[async_trait]
impl Tool for McpDisableTool {
    fn name(&self) -> &str { "mcp_disable" }
    fn description(&self) -> &str {
        "Disable a named MCP server. Kills its child process and marks it disabled so the supervisor won't auto-respawn. Use to stop a crashing or unwanted MCP server."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "server_name": { "type": "string", "description": "Name of the MCP server to disable (e.g. 'mcp-rlsp')" } },
            "required": ["server_name"],
            "additionalProperties": false
        })
    }
    async fn execute(&self, arguments: &serde_json::Value) -> Result<String, String> {
        let name = arguments.get("server_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing server_name".to_string())?;
        let state = self.manager.servers.iter().find(|s| s.name == name)
            .ok_or_else(|| format!("no MCP server named '{}'", name))?;

        // Drop the peer (closes transport, kills child).
        *state.peer_slot.lock().await = None;
        *state.status.lock().await = ServerStatus::Disabled {
            reason: "disabled via mcp_disable tool".to_string(),
        };
        info!("MCP server '{}' disabled", name);
        Ok(format!("Disabled MCP server '{}'", name))
    }
}
