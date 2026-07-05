use anyhow::{anyhow, Result};
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;

use crate::analyzer::protocol::*;

fn get_rust_analyzer_path() -> String {
    std::env::var("RUST_ANALYZER_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.cargo/bin/rust-analyzer")
    })
}

pub struct RustAnalyzerClient {
    process: Option<Child>,
    request_id: u64,
    initialized: bool,
}

impl Default for RustAnalyzerClient {
    fn default() -> Self { Self::new() }
}

impl RustAnalyzerClient {
    pub fn new() -> Self {
        Self { process: None, request_id: 0, initialized: false }
    }

    /// Spawn rust-analyzer and send the initialize handshake.
    pub async fn start(&mut self) -> Result<()> {
        let path = get_rust_analyzer_path();
        let child = tokio::process::Command::new(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        self.process = Some(child);
        self.initialize().await?;
        Ok(())
    }

    async fn initialize(&mut self) -> Result<()> {
        let current_dir = std::env::current_dir()?;
        let root_uri = format!("file://{}", current_dir.display());

        let init_params = json!({
            "processId": null,
            "clientInfo": { "name": "rust-mcp-server", "version": "0.1.0" },
            "rootUri": root_uri,
            "capabilities": {
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
        self.send_notification("initialized", json!({})).await?;
        self.initialized = true;
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

    /// Send a request and read the matching response (skipping any notifications).
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
        let header = format!("Content-Length: {}\r\n\r\n", content.len());

        if let Some(child) = &mut self.process {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(header.as_bytes()).await?;
                stdin.write_all(content.as_bytes()).await?;
                stdin.flush().await?;
            }
        }

        Ok(())
    }

    /// Read stdout until we find the JSON-RPC response with `expected_id`.
    /// Notifications emitted by rust-analyzer (e.g. publishDiagnostics) are skipped.
    async fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        if let Some(child) = &mut self.process {
            if let Some(stdout) = child.stdout.as_mut() {
                let mut reader = BufReader::new(stdout);
                loop {
                    let msg_val = read_one_message(&mut reader).await?;
                    // Is this our response?
                    if msg_val.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                        return Ok(msg_val);
                    }
                    // Otherwise it's a notification or someone else's response; skip.
                }
            }
        }
        Err(anyhow!("Failed to read response: no process/stdout"))
    }

    // ---- LSP lifecycle helpers ---------------------------------------------

    /// Send textDocument/didOpen so rust-analyzer indexes the file and emits diagnostics.
    pub async fn did_open(&mut self, abs_path: &str) -> Result<()> {
        let uri = uri_from_path(abs_path);
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
        .await
    }

    /// Send textDocument/didClose.
    pub async fn did_close(&mut self, abs_path: &str) -> Result<()> {
        let uri = uri_from_path(abs_path);
        self.send_notification(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        )
        .await
    }

    // ---- Tool implementation methods ---------------------------------------

    pub async fn find_definition(&mut self, file_path: &str, line: u32, character: u32) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let params = create_text_document_position_params(file_path, line, character);
        let response = self.send_request("textDocument/definition", params).await?;
        Ok(format_definition_response(&response))
    }

    pub async fn find_references(&mut self, file_path: &str, line: u32, character: u32) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let params = create_references_params(file_path, line, character);
        let response = self.send_request("textDocument/references", params).await?;
        Ok(format_references_response(&response))
    }

    pub async fn workspace_symbols(&mut self, query: &str) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let params = create_workspace_symbol_params(query);
        let response = self.send_request("workspace/symbol", params).await?;
        Ok(format_workspace_symbols_response(&response))
    }

    pub async fn rename_symbol(
        &mut self, file_path: &str, line: u32, character: u32, new_name: &str,
    ) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let params = create_rename_params(file_path, line, character, new_name);
        let response = self.send_request("textDocument/rename", params).await?;
        apply_workspace_edit(&response)?;
        Ok(format!("Rename applied: {new_name}"))
    }

    pub async fn get_diagnostics(&mut self, file_path: &str) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
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
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
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
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // Zero-length range at the call site; rust-analyzer matches refactor.inline
        // when the cursor is on a function call.
        let params = create_code_action_position_params(file_path, line, character);
        let response = self.send_request("textDocument/codeAction", params).await?;
        let action = pick_first_command(&response)
            .ok_or_else(|| anyhow!("No inline code action available at the given position"))?;

        execute_code_action(self, &action).await?;
        self.did_close(&abs).await?;

        Ok("Inlined function".to_string())
    }

    pub async fn organize_imports(&mut self, file_path: &str) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // Full-document range.
        let content = std::fs::read_to_string(file_path)
            .map_err(|e| anyhow!("reading {file_path}: {e}"))?;
        let line_count = content.lines().count() as u32;
        let full_range = create_range(0, 0, line_count.saturating_sub(1), u32::MAX);

        let params = create_code_action_params(
            file_path,
            full_range.clone(),
            &["source.organizeImports"],
        );

        let response = self.send_request("textDocument/codeAction", params).await?;
        let action = pick_first_command(&response)
            .ok_or_else(|| anyhow!("No organize-imports code action available"))?;

        execute_code_action(self, &action).await?;
        self.did_close(&abs).await?;

        Ok("Organized imports".to_string())
    }

    pub async fn get_type_hierarchy(
        &mut self, file_path: &str, line: u32, character: u32,
    ) -> Result<String> {
        if !self.initialized { return Err(anyhow!("Client not initialized")); }
        let abs = canonicalize(file_path);
        self.did_open(&abs).await?;

        // prepareTypeHierarchy returns an array of TypeHierarchyItems; we then request supertypes.
        let prep_params = prepare_type_hierarchy_params(file_path, line, character);
        let response = self.send_request("textDocument/prepareTypeHierarchy", prep_params).await?;
        let items = response.get("result").and_then(|r| r.as_array());
        if items.is_none() {
            return Ok(format!("No type hierarchy at {file_path}:{line}:{character}"));
        }
        let items = items.unwrap();
        if items.is_empty() {
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

async fn read_one_message(reader: &mut BufReader<&mut tokio::process::ChildStdout>) -> Result<Value> {
    // Read headers until blank line.
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

/// Pick the first code action that has a command or edit.
fn pick_first_command(response: &Value) -> Option<Value> {
    // Response is either null, an array of CodeAction objects, or an array of Command objects.
    let arr = response.get("result").and_then(|r| r.as_array()).or_else(|| response.as_array())?;
    for item in arr.iter() {
        if item.get("command").is_some() || item.get("edit").is_some() { return Some(item.clone()); }
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
    let mut line_starts: Vec<usize> = vec![0];
    {
        let mut idx = 0usize;
        for c in &chars {
            if *c == '\n' { line_starts.push(idx + 1); }
            idx += 1;
        }
    }

    // Helper to convert (line, char) -> absolute index into chars.
    fn pos_to_index(line_starts: &[usize], chars_len: usize, line: usize, col: usize) -> usize {
        if line == 0 { return std::cmp::min(col, chars_len); }
        let mut idx = *line_starts.first().unwrap_or(&0);
        for (i, start) in line_starts.iter().enumerate() {
            if i + 1 == line { idx = *start; break; }
            if i >= line { break; }
            idx = *start;
        }
        // Compute end-of-line index.
        let eol = line_starts.get(line).copied().unwrap_or(chars_len);
        std::cmp::min(idx + col, eol)
    }

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
