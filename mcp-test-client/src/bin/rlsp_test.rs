//! Standalone end-to-end test for mcp-rlsp: connects as an MCP client and
//! calls find_definition / workspace_symbols / organize_imports etc. against
//! a real rust-analyzer instance, printing raw results so we can diagnose the
//! null-results issue.
//!
//! Usage: cargo run -p mcp-test-client --bin rlsp_test [tool arg1=val1 arg2=val2 ...]
//!   e.g. rlsp_test workspace_symbols query=RustMcpServer
//!        rlsp_test find_definition file_path=mcp-rlsp/src/main.rs line=7 character=8

use anyhow::Result;
use rmcp::{
    model::CallToolRequestParams,
    service::{RoleClient},
    transport::TokioChildProcess, ClientHandler, Peer,
};

/// A client handler that logs every notification received from the server.
#[derive(Clone)]
struct LoggingClientH;

impl ClientHandler for LoggingClientH {}

async fn call(peer: &Peer<RoleClient>, name: &str, args: serde_json::Value) -> Result<String> {
    let arguments = match args {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    };
    eprintln!("[rlsp_test] >>> calling tool '{name}' with args={}", serde_json::json!(arguments));
    let r = peer
        .call_tool(CallToolRequestParams {
            name: name.to_string().into(),
            arguments,
            meta: None,
            task: None,
        })
        .await?;
    eprintln!("[rlsp_test] <<< tool result is_error={}", r.is_error.unwrap_or(false));
    let mut s = String::new();
    for c in r.content.iter() {
        if let Some(t) = c.as_text() {
            s.push_str(&t.text);
        }
    }
    Ok(s)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Build the mcp-rlsp binary path relative to workspace root.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    let rlsp_path = format!("{}/target/release/mcp-rlsp", manifest_dir);
    // Fallback: try debug build
    let path = if std::path::Path::new(&rlsp_path).exists() {
        rlsp_path
    } else {
        format!("{}/target/debug/mcp-rlsp", manifest_dir)
    };

    eprintln!("[rlsp_test] Starting mcp-rlsp at {path}");
    let cmd = tokio::process::Command::new(&path);
    // Capture stderr so we can see RA debug logs interleaved with our tool calls.
    let (transport, _stderr) = TokioChildProcess::builder(cmd).spawn()?;
    eprintln!("[rlsp_test] MCP transport spawned, initializing client...");
    let service = rmcp::serve_client(LoggingClientH, transport).await?;
    eprintln!("[rlsp_test] Client initialized.");
    let peer = service.peer().clone();

    // List tools
    let tools_result = peer.list_tools(None).await?;
    eprintln!("[rlsp_test] Available tools:");
    for t in &tools_result.tools {
        eprintln!("  - {}", t.name);
    }

    // Parse CLI args to determine which tool and what params.
    let cli_args: Vec<String> = std::env::args().skip(1).collect();
    if cli_args.is_empty() {
        eprintln!("\nNo tool specified. Usage: rlsp_test <tool_name> key=val key=val ...");
        return Ok(());
    }

    let tool_name = &cli_args[0];
    let mut params = serde_json::Map::new();
    for arg in cli_args.iter().skip(1) {
        if let Some((k, v)) = arg.split_once('=') {
            // Try parsing as number, else string.
            if let Ok(n) = v.parse::<u64>() {
                params.insert(k.to_string(), serde_json::Value::Number(n.into()));
            } else {
                params.insert(k.to_string(), serde_json::Value::String(v.to_string()));
            }
        }
    }

    eprintln!("\n[rlsp_test] Calling tool '{tool_name}' with params: {}", serde_json::json!(params));
    let result = call(&peer, tool_name, serde_json::Value::Object(params)).await?;
    println!("{result}");

    // If the result looks like null/empty/no-symbols, print a diagnostic hint.
    if result.contains("null") || result.contains("No symbols found") || result.contains("No definition found") {
        eprintln!("\n[rlsp_test] WARNING: Result appears empty/null. Possible causes:");
        eprintln!("  - RA still indexing (check stderr for progress notifications)");
        eprintln!("  - workspaceFolders/rootUri misconfiguration");
        eprintln!("  - LSP method not supported by this RA version");
    }

    Ok(())
}
