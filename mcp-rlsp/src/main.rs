use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use mcp_rlsp::server::RustMcpServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Don't call start() here — RA indexing would block MCP handshake for 90s.
    // Instead, lazy-init on first tool call via ensure_started().
    let rust_server = RustMcpServer::new();

    eprintln!("Starting mcp-rlsp server");
    eprintln!("Server running on stdio transport...");

    // Start the MCP server using the ServiceExt trait
    let service = rust_server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
