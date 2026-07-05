//! Standalone end-to-end test for mcp-loader: acts as an MCP client (exactly like
//! the bot does), connects to mcp-loader, then exercises mcp_load / mcp_list /
//! mcp_call / mcp_unload against the real time + pdf sub-servers — proving the
//! dynamic load/call chain works without any restart.

use anyhow::Result;
use rmcp::{
    model::CallToolRequestParams, service::RoleClient, transport::TokioChildProcess, ClientHandler,
    Peer,
};

#[derive(Clone)]
struct ClientH;
impl ClientHandler for ClientH {}

async fn call(peer: &Peer<RoleClient>, name: &str, args: serde_json::Value) -> Result<String> {
    let arguments = match args {
        serde_json::Value::Object(m) => Some(m),
        _ => None,
    };
    let r = peer
        .call_tool(CallToolRequestParams {
            name: name.to_string().into(),
            arguments,
            meta: None,
            task: None,
        })
        .await?;
    let mut s = String::new();
    for c in r.content.iter() {
        if let Some(t) = c.as_text() {
            s.push_str(&t.text);
        }
    }
    if r.is_error.unwrap_or(false) {
        Ok(format!("ERROR: {}", s))
    } else {
        Ok(s)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = "/home/ayourtch/ayourtch/rust/open-strix-rust";
    let loader = format!("{}/mcp-loader/target/debug/mcp-loader", base);
    let time_srv = format!("{}/mcp-time-server/target/debug/mcp-time-server", base);
    let pdf_srv = format!("{}/mcp-pdf-server/target/debug/mcp-pdf-server", base);
    let arxiv = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://arxiv.org/pdf/2606.05104".to_string());

    // Connect to the loader (as the bot would).
    let cmd = tokio::process::Command::new(&loader);
    let (transport, _stderr) = TokioChildProcess::builder(cmd).spawn()?;
    let service = rmcp::serve_client(ClientH, transport).await?;
    let peer = service.peer().clone();

    let tools = peer.list_all_tools().await?;
    println!(
        "== loader exposes: {:?}",
        tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>()
    );

    println!("\n== mcp_load time-server:");
    println!(
        "{}",
        call(
            &peer,
            "mcp_load",
            serde_json::json!({"name":"time","command":[time_srv]})
        )
        .await?
    );

    println!("\n== mcp_call time.get_current_time:");
    println!(
        "{}",
        call(
            &peer,
            "mcp_call",
            serde_json::json!({"server":"time","tool":"get_current_time","arguments":{}})
        )
        .await?
    );

    println!("\n== mcp_load pdf-server:");
    println!(
        "{}",
        call(
            &peer,
            "mcp_load",
            serde_json::json!({"name":"pdf","command":[pdf_srv]})
        )
        .await?
    );

    println!("\n== mcp_list:");
    println!("{}", call(&peer, "mcp_list", serde_json::json!({})).await?);

    println!("\n== mcp_call pdf.read_pdf {}:", arxiv);
    let pdf_text = call(
        &peer,
        "mcp_call",
        serde_json::json!({"server":"pdf","tool":"read_pdf","arguments":{"url":arxiv,"max_chars":1200}}),
    )
    .await?;
    println!("{}", pdf_text);

    println!("\n== mcp_unload pdf:");
    println!(
        "{}",
        call(&peer, "mcp_unload", serde_json::json!({"name":"pdf"})).await?
    );

    println!("\n== mcp_list after unload:");
    println!("{}", call(&peer, "mcp_list", serde_json::json!({})).await?);

    println!("\n== ALL STEPS OK ==");
    Ok(())
}
