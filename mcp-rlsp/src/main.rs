use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use mcp_rlsp::server::RustMcpServer;

#[tokio::main]
async fn main() -> Result<()> {
    let rust_server = RustMcpServer::new();

    eprintln!("Starting mcp-rlsp server (RA will spawn lazily on first tool call)");

    // Don't eagerly start RA here — indexing takes 200+ seconds and would
    // block the MCP handshake. Each tool handler calls ensure_started()
    // which spawns RA + waits for indexing before executing the query.
    let service = rust_server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
