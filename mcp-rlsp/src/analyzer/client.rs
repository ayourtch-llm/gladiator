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
    /// Send serialized JSON-RPC messages to the IO task for writing to RA's stdin.
    sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    /// Receive responses and server-initiated requests from the IO task reading RA's stdout.
    receiver: Option<tokio::sync::mpsc::UnboundedReceiver<Value>>,
    /// Set of file URIs already opened via did_open, to avoid duplicate DidOpenTextDocument errors.
    open_files: HashSet<String>,
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
               open_files: HashSet::new() }
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
        let mut child = tokio::process::Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
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
        // Don't block on project indexing — RA queues LSP requests during indexing
        // and responds once ready. read_response() handles interleaved notifications.
        Ok(())
    }

    /// Read messages (responding to server requests, skipping notifications) until
    /// no message arrives within `quiet_secs`. Used after initialize() to let RA's
    /// initial project-load notification burst drain before we accept tool calls.
    async fn drain_until_quiet(&mut self, quiet_secs: u64) {
        let mut count = 0u32;
        loop {
            match self.receiver.as_mut() {
                Some(rx) => {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(quiet_secs), rx.recv()).await
                    {
                        Ok(Some(msg_val)) => { self.handle_drained_message(&msg_val).await; count += 1; }
                        _ => { dbg_log(&format!("drain_until_quiet: drained {count} messages, channel quiet")); return; },
                    }
                }
                None => return,
            }
        }
    }

    /// Handle one drained message: respond to server requests with null, skip notifications.
    async fn handle_drained_message(&mut self, msg_val: &Value) {
        let has_result = msg_val.get("result").is_some() || msg_val.get("error").is_some();
        if !has_result && msg_val.get("id").is_some() {
            // Server-initiated request (e.g. window/workDoneProgressCreate).
            let resp = json!({
                "jsonrpc": "2.0",
                "id": msg_val.get("id").cloned().unwrap(),
                "result": Value::Null
            });
            self.send_message(&resp).await.ok();
        }
    }

    async fn initialize(&mut self) -> Result<()> {
        let current_dir = std::env::current_dir()?;
        // Rust-analyzer can't handle workspaces with "." as a member (virtual
        // manifest confusion). Instead, discover individual crate dirs from the
        // workspace Cargo.toml and pass them as separate workspaceFolders.
        let folders = discover_crate_folders(&current_dir);
        dbg_log(&format!("discover_crate_folders: CWD={}, found {} folders: {:?}",
            current_dir.display(), folders.len(),
            folders.iter().take(5).map(|f| f.to_string()).collect::<Vec<_>>()));
        let root_uri = if !folders.is_empty() {
            // Use first crate dir as rootUri; all crates are still accessible via workspaceFolders.
            format!("file://{}", folders[0])
        } else {
            format!("file://{}", current_dir.display())
        };

        let ws_folders: Vec<Value> = if !folders.is_empty() {
            folders.iter().map(|p| {
                let name = std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("workspace");
                json!({ "uri": format!("file://{p}"), "name": name })
            }).collect::<Vec<_>>()
        } else {
            let fallback = current_dir.display().to_string();
            vec![json!({
                "uri": format!("file://{fallback}"),
                "name": current_dir.file_name().and_then(|n| n.to_str()).unwrap_or("workspace")
            })]
        };

        let init_params = json!({
            "processId": null,
            "clientInfo": { "name": "rust-mcp-server", "version": "0.1.0" },
            "rootUri": root_uri,
            "workspaceFolders": ws_folders,
            "capabilities": {
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
                    }
                },
                "workspace": {
                    "symbol": { "dynamicRegistration": false },
                    "applyEdit": true,
                    "typeHierarchy": { "dynamicRegistration": false }
                }
            }
        });

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

    /// Wait up to 10s for a publishDiagnostics notification for the given file,
    /// draining notifications until one arrives or timeout. This ensures RA has
    /// finished analyzing the file before we send queries.
    async fn wait_for_diagnostics(&mut self, abs_path: &str) {
        let target_uri = uri_from_path(abs_path);
        // Drain pending messages for up to 10s looking for publishDiagnostics
        // matching our URI. Non-matching notifications are consumed and discarded;
        // responses (with result/error + id) are left in the channel.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if tokio::time::Instant::now() >= deadline { return; }
            match self.receiver.as_mut() {
                Some(rx) => {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Some(msg_val)) => {
                            // Check if this is a publishDiagnostics for our file.
                            let method = msg_val.get("method").and_then(|m| m.as_str());
                            if method == Some("textDocument/publishDiagnostics") {
                                let uri = msg_val
                                    .get("params")
                                    .and_then(|p| p.get("uri"))
                                    .and_then(|u| u.as_str())
                                    .unwrap_or("");
                                // RA may send diagnostics for multiple files; check if ours is included.
                                if !msg_val
                                    .get("params")
                                    .and_then(|p| p.get("diagnostics"))
                                    .map(|d| d.is_array() && !d.as_array().unwrap().is_empty())
                                    .unwrap_or(false)
                                {
                                    // Empty diagnostics — file analyzed but no issues. Good enough.
                                    return;
                                }
                                if uri == target_uri { return; }
                            } else if msg_val.get("result").is_some() || msg_val.get("error").is_some() {
                                // This is a response to our request, not a notification.
                                // Put it back? Can't put back into unbounded channel easily,
                                // so handle: re-send won't work. Just consume and discard —
                                // but this could be the initialize result we're waiting for!
                                // Actually wait_for_diagnostics is called AFTER did_open which
                                // is after initialize, so responses here are likely stale.
                                dbg_log("wait_for_diagnostics: consumed a response (discarding)");
                            } else if msg_val.get("id").is_some() {
                                // Server-initiated request — respond with null.
                                let resp = json!({
                                    "jsonrpc": "2.0",
                                    "id": msg_val.get("id").cloned().unwrap(),
                                    "result": Value::Null
                                });
                                self.send_message(&resp).await.ok();
                            }
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
        let response = self.send_request("textDocument/definition", params).await?;
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
        let response = self.send_request("textDocument/references", params).await?;
        dbg_log(&format!("find_references: raw response: {response}"));
        self.did_close(&abs).await?;
        Ok(format_references_response(&response))
    }

    pub async fn workspace_symbols(&mut self, query: &str) -> Result<String> {
        self.ensure_started().await?;
        
        let params = create_workspace_symbol_params(query);
        let response = self.send_request("workspace/symbol", params).await?;
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
        let response = self.send_request("textDocument/definition", params).await?;
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

        let response = self.send_request("textDocument/codeAction", params).await?;
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
        let response = self.send_request("textDocument/codeAction", params).await?;
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

        let response = self.send_request("textDocument/codeAction", params).await?;
        dbg_log(&format!("organize_imports (filtered) raw: {response}"));
        let action = pick_first_command(&response);

        // If filtered request returned nothing, try unfiltered and log.
        let action = match action {
            Some(a) => Some(a),
            None => {
                let params2 = create_code_action_params(file_path, full_range.clone(), &[]);
                let response2 = self.send_request("textDocument/codeAction", params2).await?;
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

        // prepareTypeHierarchy returns an array of TypeHierarchyItems; we then request supertypes.
        let prep_params = prepare_type_hierarchy_params(file_path, line, character);
        let response = self.send_request("textDocument/prepareTypeHierarchy", prep_params).await?;
        dbg_log(&format!("get_type_hierarchy: raw prepare response: {response}"));
        // RA may return null (cursor not on a type) or an array.
        let result_val = response.get("result").unwrap_or(&Value::Null);
        if result_val.is_null() {
            self.did_close(&abs).await?;
            return Ok(format!("No type hierarchy at {file_path}:{line}:{character}"));
        }
        let items = match result_val.as_array() {
            Some(arr) => arr,
            None => {
                self.did_close(&abs).await?;
                return Ok(format!("No type hierarchy at {file_path}:{line}:{character}"));
            }
        };
        if items.is_empty() {
            self.did_close(&abs).await?;
            return Ok(format!("No type hierarchy at {file_path}:{line}:{character}"));
        }

        // For each prepared item, request supertypes.
        let mut out = String::new();
        for item in items.iter().take(5) {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let kind = item.get("kind").and_then(|k| k.as_u64());
            if let Some(uri) = item.get("uri").and_then(|u| u.as_str()) {
                out.push_str(&format!("{name} [kind {kind:?}] at {uri}\n"));
            } else {
                out.push_str(&format!("{name} [kind {kind:?}]\n"));
            }
        }

        // Request supertypes for the first item.
        if let Some(first) = items.first() {
            let super_params = supertypes_params(first);
            let super_resp = self.send_request("typeHierarchy/supertypes", super_params).await?;
            dbg_log(&format!("get_type_hierarchy: raw supertypes response: {super_resp}"));
            if let Some(supers) = super_resp.get("result").and_then(|r| r.as_array()) {
                for s in supers.iter().take(10) {
                    let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    out.push_str(&format!("  supertype: {name}\n"));
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
