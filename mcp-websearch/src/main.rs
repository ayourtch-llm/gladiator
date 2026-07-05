use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;

fn tool_error(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

macro_rules! try_tool {
    ($expr:expr, $msg:literal) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(tool_error(format!("{}: {}", $msg, e))),
        }
    };
}

// ── Parameters ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WebSearchParams {
    /// Search query string (required).
    query: String,
    /// Maximum number of results to return. Default: 5, max: 10.
    #[serde(default)]
    limit: Option<u32>,
    /// Topic category: "general" (default), "news", or "finance".
    #[serde(default)]
    topic: Option<String>,
    /// Time range filter: "day", "week", "month", or "year". Optional.
    #[serde(default)]
    time_range: Option<String>,
}

// ── Server ───────────────────────────────────────────────────────

#[derive(Clone)]
struct WebSearchServer {
    tool_router: ToolRouter<Self>,
}

impl WebSearchServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl WebSearchServer {
    #[tool(
        description = "Search the web via Tavily API and return compact results. Requires TAVILY_API_KEY environment variable."
    )]
    async fn web_search(
        &self,
        Parameters(params): Parameters<WebSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query.trim().to_string();
        if query.is_empty() {
            return Ok(tool_error("query cannot be empty"));
        }

        let api_key = std::env::var("TAVILY_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return Ok(tool_error(
                "web_search is disabled (set TAVILY_API_KEY env var)",
            ));
        }

        let limit = params.limit.unwrap_or(5).min(10).max(1);
        let topic = params.topic.as_deref().unwrap_or("general");
        match topic {
            "general" | "news" | "finance" => {}
            _ => return Ok(tool_error(format!(
                "invalid topic '{}'. Must be one of: general, news, finance",
                topic
            ))),
        }

        if let Some(tr) = &params.time_range {
            let tr_lower = tr.to_lowercase();
            if !["day", "week", "month", "year"].contains(&tr_lower.as_str()) {
                return Ok(tool_error(
                    "time_range must be one of: day, week, month, year",
                ));
            }
        }

        // Build Tavily API request payload.
        let mut payload = serde_json::json!({
            "query": query,
            "topic": topic,
            "max_results": limit,
            "search_depth": "basic",
            "include_answer": false,
            "include_raw_content": false,
            "include_images": false,
        });
        if let Some(tr) = &params.time_range {
            payload["time_range"] = serde_json::json!(tr.to_lowercase());
        }

        let search_url =
            std::env::var("TAVILY_SEARCH_URL").unwrap_or_else(|_| "https://api.tavily.com/search".to_string());

        // Use blocking HTTP via tokio's spawn_blocking since reqwest async isn't available.
        let client = match tokio::task::spawn_blocking(move || {
            ureq::post(&search_url)
                .set("Authorization", &format!("Bearer {}", api_key))
                .send_json(payload)
        })
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => return Ok(tool_error(format!("HTTP request failed: {e}"))),
            Err(e) => return Ok(tool_error(format!("task join error: {e}"))),
        };

        let body = match client.into_string() {
            Ok(s) => s,
            Err(e) => return Ok(tool_error(format!("reading response body: {e}"))),
        };

        let parsed: serde_json::Value =
            try_tool!(serde_json::from_str(&body), "failed to parse Tavily JSON");

        let empty: Vec<serde_json::Value> = vec![];
        let rows = match parsed.get("results") {
            Some(serde_json::Value::Array(r)) => r,
            _ => &empty,
        };

        let mut compact_results: Vec<String> = Vec::new();
        for (idx, item) in rows.iter().take(limit as usize).enumerate() {
            if let Some(obj) = item.as_object() {
                let title = obj.get("title").and_then(|t| t.as_str()).unwrap_or("").trim();
                let url = obj.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let mut snippet = obj
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if snippet.len() > 800 {
                    snippet.truncate(797);
                    snippet.push_str("...");
                }
                compact_results.push(format!(
                    "{}. {} — {}\n   URL: {}\n   {}",
                    idx + 1,
                    title,
                    url,
                    url,
                    snippet
                ));
            }
        }

        let count = compact_results.len();
        if count == 0 {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No results found for \"{}\"",
                query
            ))]));
        }

        // For single result, just the text. For multiple, join with newlines.
        let output = compact_results.join("\n\n");
        Ok(CallToolResult::success(vec![Content::text(output)]))
    }
}

#[tool_handler]
impl ServerHandler for WebSearchServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "MCP server providing web search via Tavily API. Requires TAVILY_API_KEY environment variable."
                    .to_string(),
            ),
        }
    }
}

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    // Load .env from CWD (gitignored, contains TAVILY_API_KEY).
    let _ = dotenvy::dotenv();

    eprintln!("Starting mcp-websearch server");

    let api_key = std::env::var("TAVILY_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("WARNING: TAVILY_API_KEY not set — web_search will return an error");
    } else {
        eprintln!("TAVILY_API_KEY is set ({} chars)", api_key.len());
    }

    let server = WebSearchServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
