use anyhow::{anyhow, Result};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

use crate::analyzer::protocol::*;

/// Debug log to stderr.
fn dbg_log(msg: &str) {
    eprintln!("[mcp-rlsp debug] {msg}");
}

fn get_rust_analyzer_path() -> String {
    std::env::var("RUST_ANALYZER_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.cargo/bin/rust-analyzer")
    })
}

pub struct RustAnalyzerClient {
    request_id: u64,
    initialized: bool,
    sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    receiver: Option<tokio::sync::mpsc::UnboundedReceiver<Value>>, 
    open_files: HashSet<String>,
    linked_projects: Vec<String>,
}

impl Default for RustAnalyzerClient {
    fn default() -> Self { Self::new() }
}

impl Drop for RustAnalyzerClient {
    fn drop(&mut self) {
        // Dropping sender signals the IO task to exit.
        let _ = self.sender.take();
    }
}

/// Read one LSP message (headers + body) from a buffered reader.
async fn read_one_message<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line.is_empty() { break; }
        if let Some(stripped) = line.strip_prefix("Content-Length:") {
            content_length = stripped.trim().parse::<usize>().ok();
        }
    }

    let length = content_length.ok_or_else(|| anyhow!("missing Content-Length header"))?;
    let mut buf = vec![0u8; length];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

impl RustAnalyzerClient {
    pub fn new() -> Self {
        Self { request_id: 0, initialized: false, sender: None, receiver: None,
               open_files: HashSet::new(), linked_projects: Vec::new() }
    }

    /// Lazy initialization — call start() if not yet initialized.
    pub async fn ensure_started(&mut self) -> Result<()> {
        if !self.initialized {
            dbg_log("ensure_started: not initialized, calling start()");
            self.start().await?;
            dbg_log("ensure_started: start() completed");
        }
        Ok(())
    }

    /// Spawn rust-analyzer and start the IO task.
    pub async fn start(&mut self) -> Result<()> {
        let path = get_rust_analyzer_path();

        // If CWD has a [workspace] Cargo.toml (with or without [package]),
        // RA's project model gets confused by the dual manifest and returns null
        // for all queries. Fix: generate rust-project.json from cargo metadata,
        // pass it via linkedProjects, spawn RA in /tmp with rootUri=null so
        // auto-discovery finds no Cargo.toml.
        // When using rust-project.json, RA doesn't run cargo metadata at all — it
        // loads projects directly from the JSON, bypassing the dual-manifest issue.
        let current_dir = std::env::current_dir()?;
        dbg_log(&format!("start: CWD={}", current_dir.display()));
        let workspace_toml_path = current_dir.join("Cargo.toml");
        // Detect any Cargo.toml with [workspace] — even if it also has [package],
        // RA's project model gets confused by the dual manifest and returns null
        // for all queries. Using rust-project.json bypasses cargo metadata entirely.
        let is_workspace = workspace_toml_path.exists()
            && std::fs::read_to_string(&workspace_toml_path)
                .ok().map(|c| c.contains("[workspace]"))
                .unwrap_or(false);

        if is_workspace {
            dbg_log("start: [workspace] detected, generating rust-project.json");
            match self.generate_rust_project_json(&current_dir) {
                Ok(json_path) => {
                    self.linked_projects = vec![json_path];
                    dbg_log(&format!("start: using linkedProjects=[{:?}]", &self.linked_projects));
                }
                Err(e) => {
                    dbg_log(&format!("start: failed to generate rust-project.json: {e}, falling back"));
                }
            }
        }

        // Spawn RA in /tmp if we're using linkedProjects (so CWD has no Cargo.toml
        // for auto-discovery). Otherwise use current_dir normally.
        let ra_cwd = if !self.linked_projects.is_empty() {
            "/tmp".to_string()
        } else {
            current_dir.display().to_string()
        };
        
        let mut child = tokio::process::Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(&ra_cwd)
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();

        // IO task: owns stdin and stdout. Reads from stdout continuously,
        // writes to stdin when messages arrive on out_rx.
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut writer = stdin;

            loop {
                tokio::select! {
                    biased;
                    msg = out_rx.recv() => {
                        match msg {
                            Some(json_str) => {
                                let header = format!("Content-Length: {}\r\n\r\n", json_str.len());
                                if writer.write_all(header.as_bytes()).await.is_ok()
                                    && writer.write_all(json_str.as_bytes()).await.is_ok()
                                {
                                    let _ = writer.flush().await;
                                } else { break; }
                            }
                            None => break,
                        }
                    }
                    result = read_one_message(&mut reader) => {
                        match result {
                            Ok(msg_val) => {
                                if !in_tx.send(msg_val).is_ok() { break; }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }

            let _ = child.wait().await;
        });

        self.sender = Some(out_tx);
        self.receiver = Some(in_rx);

        // Child handle is owned by the IO task (moved into closure).
        self.initialize().await?;
        // Wait for RA to finish indexing before accepting tool calls.
        // RA sends $/progress notifications: begin → report(N/M) → end, twice
        // (first for project loading, then for file indexing). We wait until we've
        // seen at least 2 "end" progress events and the channel goes quiet.
        self.wait_for_indexing().await;
        Ok(())
    }

    /// Wait for rust-analyzer to finish project loading + indexing by tracking
    /// $/progress notifications. RA sends multiple begin/report/end cycles:
    ///   Cycle 1: discovering sysroot (quick, no N/M counts)
    ///   Cycle 2: cargo metadata (quick, reports "cargo metadata: started/finished")
    ///   Cycle 3+: file indexing (slow, reports "N/M" or crate paths)
    /// We wait until we see an "end" that follows progress reports containing
    /// "/" (indicating N/M counts), meaning actual file indexing completed.
    /// Wait for rust-analyzer to finish ALL project loading + indexing by tracking
    /// $/progress begin/end tokens. Each progress cycle has a unique token (e.g.
    /// "rustAnalyzer/Fetching", "rustAnalyzer/Roots Scanned"). We track outstanding
    /// begun-but-not-ended tokens and wait until all have ended OR a quiet period
    /// of no messages for N seconds after seeing at least one END. Also checks for
    /// the `experimental/serverStatus` quiescent=true notification.
    async fn wait_for_indexing(&mut self) {
        use std::collections::HashSet;
        let mut active_tokens: HashSet<String> = HashSet::new();
        let mut end_count = 0u32;
        // Quiet period fallback: if no messages arrive for this long after seeing
        // at least one END, assume indexing is done (some progress tokens never
        // formally send an "end" — e.g. "Roots Scanned").
        let quiet_duration = std::time::Duration::from_secs(10);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
        // Count consecutive quiet periods with active tokens still outstanding.
        // After 3 consecutive quiets (~30s of silence), treat remaining tokens as stale.
        let mut consecutive_quiets_with_active = 0u32;

        loop {
            if tokio::time::Instant::now() >= deadline {
                dbg_log("wait_for_indexing: timeout (300s), proceeding anyway");
                return;
            }
            match self.receiver.as_mut() {
                Some(rx) => {
                    // Wait up to quiet_duration for the next message.
                    match tokio::time::timeout(quiet_duration, rx.recv()).await {
                        Ok(Some(msg_val)) => {
                            let has_result = msg_val.get("result").is_some()
                                || msg_val.get("error").is_some();
                            if !has_result && msg_val.get("id").is_some() {
                                // Server-initiated request (e.g. window/workDoneProgressCreate)
                                let resp = json!({
                                    "jsonrpc": "2.0",
                                    "id": msg_val.get("id").cloned().unwrap(),
                                    "result": Value::Null
                                });
                                self.send_message(&resp).await.ok();
                            } else if !has_result {
                                let method = msg_val
                                    .get("method")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");
                                // Check for experimental/serverStatus with quiescent=true —
                                // this is RA's definitive "analysis settled" signal.
                                if method == "experimental/serverStatus"
                                    || method == "rust-analyzer/serverStatus"
                                {
                                    let quiescent = msg_val
                                        .get("params")
                                        .and_then(|p| p.get("quiescent"))
                                        .and_then(|q| q.as_bool())
                                        .unwrap_or(false);
                                    if quiescent {
                                        dbg_log("wait_for_indexing: serverStatus quiescent=true, ready");
                                        return;
                                    }
                                } else if method == "$/progress" {
                                    let params = msg_val.get("params").and_then(|p| p.as_object());
                                    let token = params
                                        .and_then(|p| p.get("token"))
                                        .map(|t| t.to_string())
                                        .unwrap_or_default();
                                    let value =
                                        msg_val.get("params").and_then(|p| p.get("value"));
                                    let kind =
                                        value.and_then(|v| v.get("kind")).and_then(|k| k.as_str()).unwrap_or("");
                                    let message = value
                                        .and_then(|v| v.get("message"))
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("");
                                    if kind == "begin" {
                                        active_tokens.insert(token.clone());
                                        dbg_log(&format!(
                                            "wait_for_indexing: BEGIN ({token}) {message}"
                                        ));
                                    } else if kind == "end" {
                                        end_count += 1;
                                        active_tokens.remove(&token);
                                        let remaining = active_tokens.len();
                                        dbg_log(&format!(
                                            "wait_for_indexing: END #{end_count} ({token}), {remaining} still active"
                                        ));
                                    } else if !message.is_empty() {
                                        let short = if message.len() > 60 { &message[..60] } else { message };
                                        // Only log every ~20th report to avoid noise
                                        dbg_log(&format!("wait_for_indexing: {kind}: {short}"));
                                    }
                                }
                            }
                        }
                        _ => {
                            // No message within quiet_duration.
                            if !active_tokens.is_empty() && end_count > 0 {
                                consecutive_quiets_with_active += 1;
                                dbg_log(&format!(
                                    "wait_for_indexing: quiet #{} but {} tokens still active",
                                    consecutive_quiets_with_active, active_tokens.len()
                                ));
                                // After 3 consecutive quiet periods (~30s of silence)
                                // with no new messages, treat remaining tokens as stale.
                                if consecutive_quiets_with_active >= 3 {
                                    dbg_log("wait_for_indexing: proceeding after 3 quiets (stale tokens)");
                                    return;
                                }
                            } else if end_count > 0 && active_tokens.is_empty() {
                                dbg_log(&format!(
                                    "wait_for_indexing: complete, {end_count} END events + all tokens ended"
                                ));
                                return;
                            }
                        }
                    }
                }
                None => return,
            }
        }
    }

    /// Generate a rust-project.json file for the workspace by running `cargo metadata`
    /// and converting it to RA's project model format. This bypasses cargo auto-discovery
    /// entirely — when linkedProjects points at this JSON, RA loads projects from it
    /// instead of discovering Cargo.toml files (which would find our virtual manifest).
    fn generate_rust_project_json(&self, workspace_root: &std::path::Path) -> Result<String> {
        // Run cargo metadata to get the full package graph.
        let output = std::process::Command::new("cargo")
            .arg("metadata")
            .arg("--format-version=1")
            .current_dir(workspace_root)
            .output()
            .map_err(|e| anyhow!("running cargo metadata: {e}"))?;
        
        if !output.status.success() {
            return Err(anyhow!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let meta: Value = serde_json::from_slice(&output.stdout)?;
        let packages = meta.get("packages")
            .and_then(|p| p.as_array())
            .ok_or_else(|| anyhow!("no packages in cargo metadata"))?;

        // Build id→index mapping for ALL packages. Cargo package ids are unique
        // (e.g., "registry+...#name@version" or "path+file:///.../Cargo.toml").
        let mut id_to_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, pkg) in packages.iter().enumerate() {
            if let Some(id) = pkg.get("id").and_then(|v| v.as_str()) {
                id_to_index.insert(id.to_string(), i);
            }
        }

        // Build resolve node map: package_id → resolved dep names + their package ids.
        // The resolve section handles version deduplication — each node's deps[].pkg
        // points to the exact resolved package id for that dependency.
        let mut resolve_deps: std::collections::HashMap<String, Vec<(String, String)>> = std::collections::HashMap::new();
        if let Some(nodes) = meta.get("resolve").and_then(|r| r.get("nodes")).and_then(|n| n.as_array()) {
            for node in nodes {
                let pkg_id = node.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mut deps_list = Vec::new();
                if let Some(deps) = node.get("deps").and_then(|d| d.as_array()) {
                    for dep in deps {
                        let dep_name = dep.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        let dep_pkg_id = dep.get("pkg").and_then(|p| p.as_str()).unwrap_or("").to_string();
                        // Skip dev-deps: check if this dep appears only in dev-dependencies.
                        // The resolve node includes all deps; we filter by matching against
                        // the package's own dependency list for kind filtering later.
                        deps_list.push((dep_name, dep_pkg_id));
                    }
                }
                resolve_deps.insert(pkg_id, deps_list);
            }
        }

        // Build per-package dev-dep name set to exclude from crate graph.
        let mut pkg_dev_deps: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
        for pkg in packages {
            if let Some(pkg_id) = pkg.get("id").and_then(|v| v.as_str()) {
                let mut dev_set = std::collections::HashSet::new();
                if let Some(deps) = pkg.get("dependencies").and_then(|d| d.as_array()) {
                    for dep in deps {
                        if dep.get("kind").and_then(|k| k.as_str()).unwrap_or("") == "dev" {
                            if let Some(name) = dep.get("name").and_then(|n| n.as_str()) {
                                dev_set.insert(name.to_string());
                            }
                        }
                    }
                }
                pkg_dev_deps.insert(pkg_id.to_string(), dev_set);
            }
        }

        let mut crates: Vec<Value> = Vec::new();
        for pkg in packages {
            let is_local = pkg.get("source").and_then(|s| s.as_str()).unwrap_or("") == "";
            let pkg_id = pkg.get("id").and_then(|v| v.as_str()).unwrap_or("");

            // Find root_module (prefer lib target, then first with src_path)
            let targets = pkg.get("targets")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            let mut root_module: Option<String> = None;
            for target in &targets {
                let kinds = target.get("kind").and_then(|k| k.as_array()).cloned().unwrap_or_default();
                if kinds.iter().any(|k| k == "lib") || (root_module.is_none() && !target["src_path"].is_null()) {
                    root_module = Some(
                        target.get("src_path")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string()
                    );
                }
            }

            // Build deps list using resolve nodes for correct version resolution.
            let mut crate_deps: Vec<Value> = Vec::new();
            if let Some(resolved) = resolve_deps.get(&pkg_id.to_string()) {
                let dev_set = pkg_dev_deps.get(&pkg_id.to_string());
                for (dep_name, dep_pkg_id) in resolved {
                    // Skip dev-dependencies
                    if let Some(devs) = dev_set {
                        if devs.contains(dep_name) { continue; }
                    }
                    if let Some(&dep_idx) = id_to_index.get(dep_pkg_id.as_str()) {
                        crate_deps.push(json!({
                            "crate": dep_idx,
                            "name": dep_name.replace('-', "_"),
                        }));
                    }
                }
            }

            let edition_val = pkg.get("edition")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "2024".to_string());

            let name_for_display = pkg.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();

            crates.push(json!({
                "display_name": name_for_display,
                "root_module": root_module.clone().unwrap_or_default(),
                "edition": edition_val,
                "deps": crate_deps,
                "is_workspace_member": is_local,
            }));
        }

        let project_json = json!({
            "sysroot": null,
            "crates": crates,
        });

        // Write to a temp file — must be named exactly "rust-project.json"
        let project_dir = std::env::temp_dir().join("gladiator-ra");
        std::fs::create_dir_all(&project_dir).ok();
        let project_path = project_dir.join("rust-project.json");
        std::fs::write(&project_path, serde_json::to_string_pretty(&project_json)?)
            .map_err(|e| anyhow!("writing rust-project.json: {e}"))?;
        
        dbg_log(&format!(
            "generate_rust_project_json: wrote {} crates ({} local, {} external) to {:?}",
            packages.len(),
            crates.iter().filter(|c| c.get("is_workspace_member").and_then(|v| v.as_bool()).unwrap_or(false)).count(),
            crates.iter().filter(|c| !c.get("is_workspace_member").and_then(|v| v.as_bool()).unwrap_or(true)).count(),
            project_path
        ));
        
        Ok(project_path.display().to_string())
    }

    fn capabilities_json(&self) -> Value {
        json!({
            "window": { "workDoneProgress": true },
            "textDocument": {
                "synchronization": { "didOpen": true, "didChange": false, "willSave": false, "save": false },
                "definition": { "dynamicRegistration": false },
                "references": { "dynamicRegistration": false },
                "publishDiagnostics": { "relatedInformation": true },
                "codeAction": {
                    "dynamicRegistration": false,
                    "codeActionLiteralsSupport": true,
                    "dataSupport": false
                },
                "typeHierarchy": { "dynamicRegistration": false }
            },
            "workspace": {
                "symbol": { "dynamicRegistration": false },
                "applyEdit": true
            }
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        let init_params = if !self.linked_projects.is_empty() {
            json!({
                "processId": null,
                "clientInfo": { "name": "rust-mcp-server", "version": "0.1.0" },
                "rootUri": Value::Null,
                "capabilities": self.capabilities_json(),
                "initializationOptions": {
                    "linkedProjects": self.linked_projects
                }
            })
        } else {
            let current_dir = std::env::current_dir()?;
            json!({
                "processId": null,
                "clientInfo": { "name": "rust-mcp-server", "version": "0.1.0" },
                "rootUri": format!("file://{}", current_dir.display()),
                "capabilities": self.capabilities_json(),
            })
        };

        let _response = self.send_request("initialize", init_params).await?;
        dbg_log(&format!("initialize response capabilities keys: {:?}",
            _response.get("result").and_then(|r| r.get("capabilities")).and_then(|c| c.as_object()).map(|o| o.keys().collect::<Vec<_>>())));
        self.send_notification("initialized", json!({})).await?;
        self.initialized = true;
        // Don't block on project indexing — RA queues LSP requests during indexing
        // and responds once ready. read_response() handles interleaved notifications.
        Ok(())
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.send_message(&msg).await
    }

    /// Send a request, and if result is null for semantic queries (definition,
    /// references, workspace/symbol), wait briefly and retry up to 3 times.
    /// RA returns null while still indexing — this handles the case where
    /// wait_for_indexing returned early due to stale progress tokens.
    async fn send_request_retry(&mut self, method: &str, params: Value) -> Result<Value> {
        // Methods that return meaningful results (not null-as-valid-answer).
        const RETRY_METHODS: &[&str] = &[
            "textDocument/definition",
            "textDocument/references",
            "workspace/symbol",
            "textDocument/documentSymbol",
            "textDocument/codeAction",
            "textDocument/prepareTypeHierarchy",
        ];
        let should_retry = RETRY_METHODS.contains(&method);

        for attempt in 0..3 {
            if attempt > 0 {
                dbg_log(&format!("send_request_retry: retrying {method} (attempt {})", attempt + 1));
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            let response = self.send_request(method, params.clone()).await?;
            // If result is null and this is a semantic query that should return data,
            // retry — RA may still be indexing.
            if !should_retry {
                return Ok(response);
            }
            let result_is_null = response.get("result").map(|r| r.is_null()).unwrap_or(true)
                && response.get("error").is_none();
            if !result_is_null {
                return Ok(response);
            }
            dbg_log(&format!("send_request_retry: {method} returned null (attempt {})", attempt + 1));
        }
        // Return the last (null) result after exhausting retries.
        self.send_request(method, params).await
    }

    /// Send a request and read the matching response (handling server requests/notifications).
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        self.send_message(&msg).await?;
        self.read_response(id).await
    }

    async fn send_message(&mut self, message: &Value) -> Result<()> {
        let content = message.to_string();
        if let Some(tx) = &self.sender {
            tx.send(content).map_err(|_| anyhow!("IO task closed"))?;
        } else {
            return Err(anyhow!("no sender"));
        }
        Ok(())
    }

    /// Read from the channel until we find a response with `expected_id`.
    /// Server-initiated requests (e.g. window/workDoneProgressCreate) are
    /// responded to automatically; notifications are logged and skipped.
    async fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        loop {
            // Receive one message — borrow receiver only for the recv call.
            let msg_val = {
                let rx = self.receiver.as_mut().ok_or_else(|| anyhow!("no receiver"))?;
                rx.recv().await.ok_or_else(|| anyhow!("IO task closed"))?
            };

            // Is this our response? Must have matching id AND result/error.
            let has_result = msg_val.get("result").is_some() || msg_val.get("error").is_some();
            if has_result && msg_val.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                return Ok(msg_val);
            }
            if !has_result && msg_val.get("id").is_some() {
                if let Some(method) = msg_val.get("method").and_then(|m| m.as_str()) {
                    dbg_log(&format!("read_response: handling server request '{method}'"));
                    // Respond with null result to acknowledge.
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": msg_val.get("id").cloned().unwrap(),
                        "result": Value::Null
                    });
                    self.send_message(&resp).await?;
                }
            } else if !has_result && msg_val.get("method").is_some() {
                // Notification (no id) — e.g. $/progress, publishDiagnostics.
                let method = msg_val.get("method").and_then(|m| m.as_str()).unwrap_or("");
                dbg_log(&format!("read_response: skipping notification '{method}'"));
            }
        }
    }

    // ---- LSP lifecycle helpers ---------------------------------------------

    /// Send textDocument/didOpen so rust-analyzer indexes the file. Skips
    /// duplicate opens (RA errors on double DidOpenTextDocument for same URI).
    pub async fn did_open(&mut self, abs_path: &str) -> Result<()> {
        let uri = uri_from_path(abs_path);
        if !self.open_files.insert(uri.clone()) {
            // Already opened — skip.
            return Ok(());
        }
        let content = std::fs::read_to_string(abs_path)
            .map_err(|e| anyhow!("reading {abs_path}: {e}"))?;
        let lang_id = if abs_path.ends_with(".rs") { "rust" }
            else if abs_path.ends_with(".toml") { "toml" }
            else { "plaintext" };

        self.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": lang_id,
                    "version": 1,
                    "text": content
                }
            }),
        )
        .await?;
        // Wait for RA to publish diagnostics — this confirms the file is indexed.
        self.wait_for_diagnostics(&abs_path).await;
        Ok(())
    }

    /// Send textDocument/didClose and remove from open_files tracking.
    pub async fn did_close(&mut self, abs_path: &str) -> Result<()> {
        let uri = uri_from_path(abs_path);
        self.open_files.remove(&uri);
        self.send_notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }

    /// Wait up to 5s for a publishDiagnostics notification, draining notifications
    /// only. Since indexing is complete (start() drains all progress), diagnostics
    /// arrive quickly after didOpen. This confirms RA has analyzed the file.
    async fn wait_for_diagnostics(&mut self, abs_path: &str) {
        let target_uri = uri_from_path(abs_path);
        // Short timeout — with proper indexing drain in start(), publishDiagnostics
        // arrives within ~1s of didOpen.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if tokio::time::Instant::now() >= deadline { return; }
            match self.receiver.as_mut() {
                Some(rx) => {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(msg_val)) => {
                            let method = msg_val.get("method").and_then(|m| m.as_str());
                            if method == Some("textDocument/publishDiagnostics") {
                                // Any publishDiagnostics (even empty) means the file is analyzed.
                                dbg_log(&format!("wait_for_diagnostics: got publishDiagnostics for {}",
                                    msg_val.get("params")
                                        .and_then(|p| p.get("uri"))
                                        .and_then(|u| u.as_str())
                                        .unwrap_or("?")));
                                return;
                            } else if !msg_val.get("result").is_some() && !msg_val.get("error").is_some()
                                && msg_val.get("id").is_some()
                            {
                                // Server-initiated request — respond with null.
                                let resp = json!({
                                    "jsonrpc": "2.0",
                                    "id": msg_val.get("id").cloned().unwrap(),
                                    "result": Value::Null
                                });
                                self.send_message(&resp).await.ok();
                            }
                            // Skip all other notifications silently (progress, etc.)
                        }
                        _ => return,
                    }
                }
                None => return,
            }
        }
    }

    // ---- Tool implementation methods ---------------------------------------

    pub async fn find_definition(&mut self, file_path: &str, line: u32, character: u32) -> Result<String> {
        self.ensure_started().await?;
        
        let abs = canonicalize(file_path);
        dbg_log(&format!("find_definition: file={file_path} abs={abs} line={line} char={character}"));
        // Open the doc so RA can resolve cross-file definitions.
        self.did_open(&abs).await?;
        let params = create_text_document_position_params(file_path, line, character);
        dbg_log(&format!("find_definition: sending textDocument/definition with params={params}"));
        let response = self.send_request_retry("textDocument/definition", params).await?;
        dbg_log(&format!("find_definition: raw response: {response}"));
        self.did_close(&abs).await?;
        Ok(format_definition_response(&response))
    }

    pub async fn find_references(&mut self, file_path: &str, line: u32, character: u32) -> Result<String> {
        dbg_log("find_references: entry");
        self.ensure_started().await?;
        let abs = canonicalize(file_path);
        dbg_log(&format!("find_references: did_open {abs}"));
        // Open the doc so RA can resolve references (including cross-file).
        self.did_open(&abs).await?;
        dbg_log("find_references: sending textDocument/references");
        let params = create_references_params(file_path, line, character);
        let response = self.send_request_retry("textDocument/references", params).await?;
        dbg_log(&format!("find_references: raw response: {response}"));
        self.did_close(&abs).await?;
        Ok(format_references_response(&response))
    }

    pub async fn workspace_symbols(&mut self, query: &str) -> Result<String> {
        self.ensure_started().await?;
        
        // RA needs at least one did_open to trigger file analysis before it
        // can serve workspace/symbol queries. Open a known source file.
        let warmup_path = canonicalize("mcp-rlsp/src/main.rs");
        if std::path::Path::new(&warmup_path).exists() {
            self.did_open(&warmup_path).await?;
        }
        
        let params = create_workspace_symbol_params(query);
        let response = self.send_request_retry("workspace/symbol", params).await?;
        Ok(format_workspace_symbols_response(&response))
    }

    pub async fn rename_symbol(
        &mut self, file_path: &str, line: u32, character: u32, new_name: &str,
    ) -> Result<String> {
        self.ensure_started().await?;
        
        let params = create_rename_params(file_path, line, character, new_name);
        let response = self.send_request("textDocument/rename", params).await?;
        apply_workspace_edit(&response)?;
        Ok(format!("Rename applied: {new_name}"))
    }

    pub async fn get_diagnostics(&mut self, file_path: &str) -> Result<String> {
        self.ensure_started().await?;
        
        let abs = canonicalize(file_path);
        // Open the doc so rust-analyzer indexes it and emits publishDiagnostics.
        self.did_open(&abs).await?;
        // Force a round-trip: send a no-op definition request at (0,0) to flush
        // pending diagnostics notifications from the server's queue. We discard them;
        // for richer output use `cargo check` via bash.
        let params = create_text_document_position_params(file_path, 0, 0);
        let response = self.send_request_retry("textDocument/definition", params).await?;
        // Close the doc so we don't keep it open in rust-analyzer's memory.
        self.did_close(&abs).await?;

        Ok(format!("Diagnostics for {file_path}: opened and closed. Definition-at-origin response: {response}"))
    }

    pub async fn extract_function(
        &mut self, file_path: &str,
        start_line: u32, start_character: u32,
        end_line: u32, end_character: u32,
        function_name: &str,
    ) -> Result<String> {
        self.ensure_started().await?;
        
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // LSP positions are 0-based; tool args may be either. We pass them through as-is.
        let range = create_range(start_line, start_character, end_line, end_character);
        let params = create_code_action_params(
            file_path,
            range.clone(),
            &["refactor.extract.function"],
        );

        let response = self.send_request_retry("textDocument/codeAction", params).await?;
        dbg_log(&format!("extract_function: raw codeAction response: {response}"));
        let action = pick_first_command(&response)
            .ok_or_else(|| anyhow!("No extract function code action available for the given range"))?;

        execute_code_action(self, &action).await?;
        // LSP extract doesn't accept a target name; the function_name arg is advisory only.
        let _ = function_name;
        self.did_close(&abs).await?;

        Ok("Extracted function".to_string())
    }

    pub async fn inline_function(
        &mut self, file_path: &str, line: u32, character: u32,
    ) -> Result<String> {
        self.ensure_started().await?;
        
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // Zero-length range at the call site; rust-analyzer matches refactor.inline
        // when the cursor is on a function call.
        let params = create_code_action_position_params(file_path, line, character);
        let response = self.send_request_retry("textDocument/codeAction", params).await?;
        dbg_log(&format!("inline_function: raw codeAction response: {response}"));
        let action = pick_first_command(&response)
            .ok_or_else(|| anyhow!("No inline code action available at the given position"))?;

        execute_code_action(self, &action).await?;
        self.did_close(&abs).await?;

        Ok("Inlined function".to_string())
    }

    pub async fn organize_imports(&mut self, file_path: &str) -> Result<String> {
        self.ensure_started().await?;
        
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // Full-document range. End character must be a valid UTF-16 offset,
        // not u32::MAX (which RA rejects as out-of-bounds).
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| anyhow!("reading {file_path}: {e}"))?;
        let line_count = content.lines().count() as u32;
        let last_line = line_count.saturating_sub(1);
        // Compute the actual char length of the last line.
        let last_line_len = content.lines().last().map(|l| l.chars().count()).unwrap_or(0) as u32;
        dbg_log(&format!("organize_imports: file={file_path} lines={line_count} last_line={last_line} last_len={last_line_len}"));
        let full_range = create_range(0, 0, last_line, last_line_len);

        // First try with source.organizeImports; if empty, retry without "only"
        // filter so we can inspect what RA returns.
        let params = create_code_action_params(
            file_path,
            full_range.clone(),
            &["source.organizeImports"],
        );

        let response = self.send_request_retry("textDocument/codeAction", params).await?;
        dbg_log(&format!("organize_imports (filtered) raw: {response}"));
        let action = pick_first_command(&response);

        // If filtered request returned nothing, try unfiltered and log.
        let action = match action {
            Some(a) => Some(a),
            None => {
                let params2 = create_code_action_params(file_path, full_range.clone(), &[]);
                let response2 = self.send_request_retry("textDocument/codeAction", params2).await?;
                dbg_log(&format!("organize_imports (unfiltered) raw: {response2}"));
                pick_first_command_with_kind(&response2, "source.organizeImports")
            }
        };

        let action = action
            .ok_or_else(|| anyhow!("No organize-imports code action available"))?;

        execute_code_action(self, &action).await?;
        self.did_close(&abs).await?;

        Ok("Organized imports".to_string())
    }

    pub async fn get_type_hierarchy(
        &mut self, file_path: &str, line: u32, character: u32,
    ) -> Result<String> {
        self.ensure_started().await?;

        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // rust-analyzer doesn't implement textDocument/prepareTypeHierarchy.
        // Fallback: use hover to identify the symbol at cursor, then
        // workspace/symbol + implementationProvider to find related types.

        // 1. Hover at position to get type info (name, kind).
        let hover_params = create_text_document_position_params(file_path, line, character);
        let hover_resp = self.send_request_retry("textDocument/hover", hover_params).await?;
        dbg_log(&format!("get_type_hierarchy: raw hover response: {hover_resp}"));

        let hover_result = hover_resp.get("result").unwrap_or(&Value::Null);
        if hover_result.is_null() {
            self.did_close(&abs).await?;
            return Ok(format!("No type hierarchy at {file_path}:{line}:{character} (cursor not on a symbol)"));
        }

        // Extract the hovered symbol name from hover markup content.
        let mut raw_hover = String::new();
        if let Some(hover_obj) = hover_result.as_object() {
            if let Some(content_val) = hover_obj.get("contents") {
                if let Some(s) = content_val.as_str() {
                    raw_hover = s.to_string();
                } else if let Some(obj) = content_val.as_object() {
                    if let Some(val) = obj.get("value").and_then(|v| v.as_str()) {
                        raw_hover = val.to_string();
                    }
                } else if let Some(arr) = content_val.as_array() {
                    for item in arr {
                        if let Some(s) = item.as_str() { raw_hover.push_str(s); break; }
                        if let Some(obj) = item.as_object() {
                            if let Some(val) = obj.get("value").and_then(|v| v.as_str()) {
                                raw_hover = val.to_string(); break;
                            }
                        }
                    }
                }
            }
        }

        // Parse the short type name from hover content.
        let symbol_name = {
            let mut found = String::new();
            for line in raw_hover.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ") {
                    let after_kw = if trimmed.starts_with("pub struct ") { &trimmed[12..] }
                                   else { &trimmed[8..] };
                    found = after_kw.split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next().unwrap_or("").to_string();
                    break;
                }
                if trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ") {
                    let after_kw = if trimmed.starts_with("pub enum ") { &trimmed[9..] }
                                   else { &trimmed[5..] };
                    found = after_kw.split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next().unwrap_or("").to_string();
                    break;
                }
            }
            // Fallback: last segment of first line (handles "module::path::Type")
            if found.is_empty() {
                let first_line = raw_hover.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                found = first_line.rsplit("::").next().unwrap_or(first_line).trim()
                    .to_string();
            }
            // If it's a primitive like "u64", that's not useful for type hierarchy
            if found.is_empty() || found.len() <= 2 {
                self.did_close(&abs).await?;
                return Ok(format!("No type hierarchy at {file_path}:{line}:{character} (cursor on '{found}', not a struct/enum/trait)"));
            }
            found
        };

        dbg_log(&format!("get_type_hierarchy: extracted symbol_name={symbol_name}"));

        // 2. Use workspace/symbol to find all matching symbols (structs, traits, impls).
        let ws_params = create_workspace_symbol_params(&symbol_name);
        let ws_resp = self.send_request_retry("workspace/symbol", ws_params).await?;
        dbg_log(&format!("get_type_hierarchy: raw workspace/symbol response length={ws_resp}"));

        let mut out = format!("{symbol_name}\n");
        if let Some(symbols) = ws_resp.get("result").and_then(|r| r.as_array()) {
            // Group by symbol kind
            for sym in symbols.iter().take(20) {
                let name = sym.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let kind = sym.get("kind").and_then(|k| k.as_u64());
                let container = sym.get("containerName").and_then(|c| c.as_str()).unwrap_or("");

                // LSP SymbolKind: 23=Struct, 24=Enum, 25=Operator, 26=TypeParameter,
                // 5=Class, 6=Function, 11=Constructor, 2=Module
                let kind_str = match kind {
                    Some(1) => "File", Some(2) => "Module", Some(3) => "Namespace",
                    Some(4) => "Package", Some(5) => "Class", Some(6) => "Method/Function",
                    Some(7) => "Property", Some(8) => "Field", Some(9) => "Enum",
                    Some(10) => "Interface", Some(11) => "Function", Some(12) => "String",
                    Some(13) => "Number", Some(14) => "Boolean", Some(15) => "Array",
                    Some(20) => "TypeParameter", Some(22) => "Struct", Some(23) => "Enum",
                    _ => "?"
                };

                if let Some(loc) = sym.get("location") {
                    let uri = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    out.push_str(&format!("  {name} [{kind_str}] in {container} at {uri}\n"));
                } else {
                    out.push_str(&format!("  {name} [{kind_str}] in {container}\n"));
                }
            }

            // Find impl blocks — these show trait implementations (supertypes)
            let impls: Vec<&Value> = symbols.iter()
                .filter(|s| s.get("name").and_then(|n| n.as_str()).unwrap_or("").contains(&symbol_name))
                .collect();
            if !impls.is_empty() {
                out.push_str("\nImplementations:\n");
                for imp in impls.iter().take(10) {
                    let name = imp.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let container = imp.get("containerName").and_then(|c| c.as_str()).unwrap_or("");
                    out.push_str(&format!("  {name} (in {container})\n"));
                }
            }
        }

        // 3. Use textDocument/implementation to find implementations of this type.
        let impl_params = create_text_document_position_params(file_path, line, character);
        let impl_resp = self.send_request("textDocument/implementation", impl_params).await?;
        dbg_log(&format!("get_type_hierarchy: raw implementation response: {impl_resp}"));
        if let Some(impls) = impl_resp.get("result").and_then(|r| r.as_array()) {
            if !impls.is_empty() {
                out.push_str("\nImplementations at cursor:\n");
                for imp in impls.iter().take(10) {
                    let uri = imp.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    if let Some(range) = imp.get("range") {
                        let start_line = range.get("start").and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0);
                        out.push_str(&format!("  {uri}:{start_line}\n"));
                    }
                }
            }
        }

        self.did_close(&abs).await?;

        Ok(out)
    }
}

// ---- free helper functions -------------------------------------------------

/// Execute a CodeAction either by workspace/executeCommand (if it has a command)
/// or by applying its edit directly.
async fn execute_code_action(client: &mut RustAnalyzerClient, action: &Value) -> Result<()> {
    if let Some(cmd) = action.get("command").and_then(|c| c.as_str()) {
        let args = action.get("arguments").cloned().unwrap_or(json!([]));
        client
            .send_request(
                "workspace/executeCommand",
                json!({ "command": cmd, "arguments": args }),
            )
            .await?;
    } else if let Some(edit) = action.get("edit") {
        apply_workspace_edit(&json!({"result": edit}))?;
    }
    Ok(())
}

/// Pick the first code action that has a command or edit.
fn pick_first_command(response: &Value) -> Option<Value> {
    // Response is either null, an array of CodeAction objects, or an array of Command objects.
    let arr = response.get("result").and_then(|r| r.as_array()).or_else(|| response.as_array())?;
    for item in arr.iter() {
        if item.get("command").is_some() || item.get("edit").is_some() { return Some(item.clone()); }
    }
    None
}

/// Pick the first code action whose kind matches `kind`.
fn pick_first_command_with_kind(response: &Value, kind: &str) -> Option<Value> {
    let arr = response.get("result").and_then(|r| r.as_array()).or_else(|| response.as_array())?;
    for item in arr.iter() {
        if item.get("kind").and_then(|k| k.as_str()) == Some(kind)
            && (item.get("command").is_some() || item.get("edit").is_some())
        {
            return Some(item.clone());
        }
    }
    None
}

/// Apply a WorkspaceEdit returned by rust-analyzer to the local files on disk.
fn apply_workspace_edit(response: &Value) -> Result<()> {
    // The edit may be at response["result"]["changes"] or response["result"]["documentChanges"]
    // depending on whether it came from a request result or an inline action object.
    let edits_obj = response
        .get("result")
        .or_else(|| Some(response))
        .unwrap_or(&Value::Null);

    if let Some(changes) = edits_obj.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits_val) in changes.iter() {
            if let Some(edits_arr) = edits_val.as_array() {
                apply_edits_to_uri(uri, edits_arr)?;
            }
        }
    } else if let Some(doc_changes) = edits_obj
        .get("documentChanges")
        .or_else(|| response.get("documentChanges"))
        .and_then(|c| c.as_array())
    {
        for change in doc_changes.iter() {
            // Each entry is either { kind: "create"/"delete"/"rename", uri } or a full edit:
            // { textDocument: {uri}, edits: [...] }
            if let Some(edits_arr) = change.get("edits").and_then(|e| e.as_array()) {
                if let Some(uri) = change
                    .get("textDocument")
                    .and_then(|t| t.get("uri"))
                    .and_then(|u| u.as_str())
                {
                    apply_edits_to_uri(uri, edits_arr)?;
                }
            } else if let Some(_op_kind) = change.get("kind").and_then(|k| k.as_str()) {
                // create/delete/rename — not handled here; would need fs ops.
            }
        }
    }

    Ok(())
}

fn apply_edits_to_uri(uri: &str, edits_arr: &[Value]) -> Result<()> {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    // Read current content.
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("reading {path}: {e}"))?;

    // Collect edits as (start_line, start_char, end_line, end_char, new_text).
    let mut collected: Vec<(usize, usize, usize, usize, String)> = Vec::new();
    for edit in edits_arr.iter() {
        if let Some(range) = edit.get("range") {
            let sl = range["start"]["line"].as_u64().unwrap_or(0) as usize;
            let sc = range["start"]["character"].as_u64().unwrap_or(0) as usize;
            let el = range["end"]["line"].as_u64().unwrap_or(0) as usize;
            let ec = range["end"]["character"].as_u64().unwrap_or(0) as usize;
            let new_text = edit
                .get("newText")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
            collected.push((sl, sc, el, ec, new_text));
        }
    }

    // Apply edits in reverse order of start position so earlier offsets stay valid.
    collected.sort_by(|a, b| {
        let ka = a.0 * 100_000 + a.1;
        let kb = b.0 * 100_000 + b.1;
        kb.cmp(&ka)
    });

    // Convert text to Vec<char> for index-based editing.
    let mut chars: Vec<char> = text.chars().collect();
    let line_starts = build_line_starts(&chars);

    for (sl, sc, el, ec, new_text) in &collected {
        let start_idx = pos_to_index(&line_starts, chars.len(), *sl, *sc);
        let end_idx = pos_to_index(&line_starts, chars.len(), *el, *ec);
        if start_idx > end_idx { continue; }
        // Replace range [start_idx..end_idx) with new_text.
        let replacement: Vec<char> = new_text.chars().collect();
        chars.splice(start_idx..end_idx, replacement.clone());
    }

    std::fs::write(path, chars.iter().collect::<String>())
        .map_err(|e| anyhow!("writing {path}: {e}"))?;
    Ok(())
}

/// Build a Vec where index i = char-index of the start of LSP-line i.
fn build_line_starts(chars: &[char]) -> Vec<usize> {
    let mut ls = vec![0usize];
    for (i, c) in chars.iter().enumerate() { if *c == '\n' { ls.push(i + 1); } }
    ls
}

/// Convert an LSP (line, character) position to a char-index into `chars`.
fn pos_to_index(line_starts: &[usize], chars_len: usize, line: usize, col: usize) -> usize {
    let idx = *line_starts.get(line).unwrap_or(&chars_len);
    // End-of-line clamp: start of next line minus the newline char; or end of text.
    let eol = if let Some(next_start) = line_starts.get(line + 1) {
        next_start.saturating_sub(1)
    } else { chars_len };
    std::cmp::min(idx + col, eol)
}

#[cfg(test)]
mod pos_to_index_tests {
    use super::*;

    fn ls(text: &str) -> Vec<usize> {
        let cv: Vec<char> = text.chars().collect();
        build_line_starts(&cv)
    }

    #[test]
    fn line2_col0_is_third_physical_line_start() {
        // "aaa\nbbb\nccc" — LSP lines 0,1,2 start at char-indices 0,4,8
        let l = ls("aaa\nbbb\nccc");
        assert_eq!(l, vec![0, 4, 8]);
        let n = "aaa\nbbb\nccc".chars().count();
        // LSP line 2 col 0 -> index of 'c' in third physical line (index 8)
        assert_eq!(pos_to_index(&l, n, 2, 0), 8);
    }

    #[test]
    fn line1_col3_end_of_second_line() {
        let l = ls("aaa\nbbb\nccc");
        let n = "aaa\nbbb\nccc".chars().count();
        // LSP line 1 col 3 -> end of 'bbb' (index 7)
        assert_eq!(pos_to_index(&l, n, 1, 3), 4 + 3); // 7
    }

    #[test]
    fn line0_col2_first_line() {
        let l = ls("aaa\nbbb");
        let n = "aaa\nbbb".chars().count();
        assert_eq!(pos_to_index(&l, n, 0, 2), 2);
    }

    // End-to-end: simulate the rename_symbol corruption scenario.
    // main.rs content with `rust_server` on LSP-lines 7,8,14. RA returns a
    // WorkspaceEdit replacing each occurrence's range with "server".
    #[test]
    fn apply_workspace_edit_renames_all_occurrences_correctly() {
        use std::env;
        let dir = env::temp_dir();
        let path = dir.join("mcp_rlsp_rename_test_main.rs");
        let original =
            "use anyhow::Result;\n\
             use rmcp::{ServiceExt, transport::stdio};\n\
             use mcp_rlsp::server::RustMcpServer;\n\n\
             #[tokio::main]\n\
             async fn main() -> Result<()> {\n\
             // Initialize the rust-analyzer integration\n\
             let mut rust_server = RustMcpServer::new();\n\
             rust_server.start().await?;\n\n\
             eprintln!(\"Starting mcp-rlsp server\");\n\
             eprintln!(\"Server running on stdio transport...\");\n\n\
             // Start the MCP server using the ServiceExt trait\n\
             let service = rust_server.serve(stdio()).await?;\n\
             service.waiting().await?;\n\n\
             Ok(())\n}\n";
        std::fs::write(&path, original).unwrap();

        // RA-style WorkspaceEdit: changes keyed by file URI.
        let uri = format!("file://{}", path.display());
        let edit_json = serde_json::json!({
            "result": {
                "changes": {
                    &uri: [
                        { "range": { "start": {"line": 7, "character": 8}, "end": {"line": 7, "character": 19} }, "newText": "server" },
                        { "range": { "start": {"line": 8, "character": 0},  "end": {"line": 8, "character": 11} }, "newText": "server" },
                        { "range": { "start": {"line": 14, "character": 14}, "end": {"line": 14, "character": 25} }, "newText": "server" }
                    ]
                }
            }
        });

        apply_workspace_edit(&edit_json).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        // All three occurrences renamed; comments untouched.
        assert!(!after.contains("rust_server"), "rename left a `rust_server` occurrence");
        assert!(after.contains("// Initialize the rust-analyzer integration"),
            "comment corrupted: {:?}", after.lines().nth(6));
        assert!(after.contains("let mut server = RustMcpServer::new();"),
            "declaration not renamed correctly:\n{}", after);
        assert!(after.contains("server.start().await?"));
        assert!(after.contains("let service = server.serve(stdio()).await?"));

        let _ = std::fs::remove_file(&path);
    }
}

/// Discover individual crate directories from the workspace Cargo.toml.
/// Rust-analyzer can't handle workspaces with "." as a member, so we walk up
/// to find the workspace root, parse members (excluding ".") and return paths
/// to each subdirectory that has both a Cargo.toml. Falls back to [current_dir]
/// if no workspace manifest is found.
fn discover_crate_folders(current_dir: &std::path::Path) -> Vec<String> {
    // Walk up to find the nearest Cargo.toml with [workspace].
    let mut dir = current_dir.to_path_buf();
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if std::fs::read_to_string(&cargo_toml)
            .ok()
            .map(|c| c.contains("[workspace]"))
            .unwrap_or(false)
        { break; }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return vec![current_dir.display().to_string()],
        }
    }

    let workspace_root = dir;
    dbg_log(&format!("discover_crate_folders: workspace root={}", workspace_root.display()));
    let content = std::fs::read_to_string(workspace_root.join("Cargo.toml"))
        .unwrap_or_default();

    // Parse `members = [...]` list, extracting quoted paths (excluding ".").
    let mut folders = Vec::new();
    let mut in_members = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if !in_members {
            if trimmed.starts_with("members") && trimmed.contains('[') { in_members = true; }
            else { continue; }
        }

        // Extract quoted strings from the members list.
        let mut remaining = trimmed;
        while let Some(start) = remaining.find('"') {
            let after_start = &remaining[start + 1..];
            if let Some(end_rel) = after_start.find('"') {
                let member = &after_start[..end_rel];
                if !member.is_empty() && member != "." {
                    let path = workspace_root.join(member).display().to_string();
                    if std::path::Path::new(&path).join("Cargo.toml").exists() {
                        folders.push(path);
                    }
                }
                remaining = &after_start[end_rel + 1..];
            } else { break; }
        }

        if trimmed.contains(']') { break; }
    }

    if folders.is_empty() {
        vec![current_dir.display().to_string()]
    } else {
        folders
    }
}

fn canonicalize(file_path: &str) -> String {
    std::fs::canonicalize(file_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file_path.to_string())
}

// ---- response formatters ---------------------------------------------------

fn format_definition_response(response: &Value) -> String {
    let result = response.get("result").unwrap_or(&Value::Null);
    if let Some(arr) = result.as_array() {
        if arr.is_empty() { return "No definition found".to_string(); }
        let mut out = String::new();
        for loc in arr.iter() {
            if let (Some(uri), Some(range)) = (loc.get("uri").and_then(|u| u.as_str()), loc.get("range")) {
                let sl = range["start"]["line"].as_u64().unwrap_or(0);
                let sc = range["start"]["character"].as_u64().unwrap_or(0);
                out.push_str(&format!("{uri}:{sl}:{sc}\n"));
            }
        }
        if out.is_empty() { "No definition found".to_string() } else { out.trim_end().to_string() }
    } else if let Some(loc) = result.as_object() {
        if let (Some(uri), Some(range)) = (loc.get("uri").and_then(|u| u.as_str()), loc.get("range")) {
            let sl = range["start"]["line"].as_u64().unwrap_or(0);
            let sc = range["start"]["character"].as_u64().unwrap_or(0);
            format!("{uri}:{sl}:{sc}")
        } else { "No definition found".to_string() }
    } else if result.is_null() {
        "No definition found".to_string()
    } else {
        format!("Definition response: {response}")
    }
}

fn format_references_response(response: &Value) -> String {
    let result = response.get("result").unwrap_or(&Value::Null);
    if let Some(arr) = result.as_array() {
        if arr.is_empty() { return "No references found".to_string(); }
        let mut out = String::new();
        for loc in arr.iter() {
            if let (Some(uri), Some(range)) = (loc.get("uri").and_then(|u| u.as_str()), loc.get("range")) {
                let sl = range["start"]["line"].as_u64().unwrap_or(0);
                let sc = range["start"]["character"].as_u64().unwrap_or(0);
                out.push_str(&format!("{uri}:{sl}:{sc}\n"));
            }
        }
        if out.is_empty() { "No references found".to_string() } else { out.trim_end().to_string() }
    } else {
        format!("References response: {response}")
    }
}

fn format_workspace_symbols_response(response: &Value) -> String {
    let result = response.get("result").unwrap_or(&Value::Null);
    if let Some(arr) = result.as_array() {
        if arr.is_empty() { return "No symbols found".to_string(); }
        let mut out = String::new();
        for sym in arr.iter() {
            let name = sym.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let kind = sym.get("kind").and_then(|k| k.as_u64()).map(|k| k.to_string()).unwrap_or_default();
            if let Some(loc) = sym.get("location") {
                if let (Some(uri), Some(range)) = (loc.get("uri").and_then(|u| u.as_str()), loc.get("range")) {
                    let sl = range["start"]["line"].as_u64().unwrap_or(0);
                    out.push_str(&format!("{name} [kind {kind}] at {uri}:{sl}\n"));
                }
            } else if let Some(containers) = sym.get("containerName").and_then(|c| c.as_str()) {
                out.push_str(&format!("{name} [kind {kind}] in {containers}\n"));
            } else {
                out.push_str(&format!("{name} [kind {kind}]\n"));
            }
        }
        if out.is_empty() { "No symbols found".to_string() } else { out.trim_end().to_string() }
    } else {
        format!("Workspace symbols response: {response}")
    }
}
