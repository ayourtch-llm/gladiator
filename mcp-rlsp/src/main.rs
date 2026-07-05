use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use mcp_rlsp::server::RustMcpServer;

#[tokio::main]
async fn main() -> Result<()> {
    let mut rust_server = RustMcpServer::new();

    eprintln!("Starting mcp-rlsp server");

    // Spawn RA and drain its initial indexing notification burst now,
    // before accepting MCP tool calls. This blocks startup but means
    // the first tool call hits an already-indexed RA.
    eprintln!("Spawning rust-analyzer and waiting for index...");
    rust_server.start().await?;
    eprintln!("rust-analyzer ready, serving on stdio...");

    let service = rust_server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
