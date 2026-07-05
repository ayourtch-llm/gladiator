//! mcp-pdf-server — fetch a PDF over HTTP(S) and return its extracted text.
//!
//! Exposes one tool, `read_pdf(url, max_chars?)`: downloads the URL, runs the
//! host's `pdftotext` on it, and returns the extracted text (truncated to
//! max_chars, default 8000, to stay well under an LLM context window).
//!
//! Designed to be driven dynamically through mcp-loader (mcp_load this binary,
//! then mcp_call read_pdf), e.g. on arxiv papers: https://arxiv.org/pdf/<id>.

use anyhow::Result;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing_subscriber::EnvFilter;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tool_error(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}
fn tool_ok_text(s: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s)])
}

/// LLM chat-completion request (OpenAI-compatible subset).
#[derive(Debug, Serialize)]
struct LlmChatRequest {
    model: String,
    messages: Vec<LlmChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LlmChatMessage {
    role: String,
    content: String,
}

/// LLM chat-completion response (OpenAI-compatible subset).
#[derive(Debug, Deserialize)]
struct LlmChatResponse {
    choices: Vec<LlmChatChoice>,
}

#[derive(Debug, Deserialize)]
struct LlmChatChoice {
    message: LlmChatMessage,
}

/// Parameters for the analyze_pdf tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AnalyzePdfParams {
    /// PDF source: an http(s) URL or a local file path (same as read_pdf).
    url: String,
    /// Prompt / question to ask the LLM about the PDF content.
    prompt: String,
    /// Max characters of PDF text to include (default 80000).
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadPdfParams {
    /// PDF source: an http(s) URL (e.g. https://arxiv.org/pdf/2401.00001) OR a
    /// local file path (e.g. /home/user/paper.pdf).
    url: String,
    /// Character offset into the extracted text to start at (default 0). Use with
    /// the `next offset` reported in the response to page through a long document.
    #[serde(default)]
    offset: Option<usize>,
    /// Max characters to return in this call (default 8000). Pass a large value to
    /// pull the whole document at once; otherwise page through it with `offset`.
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Clone)]
struct PdfServer {
    tool_router: ToolRouter<Self>,
}
impl PdfServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl PdfServer {
    #[tool(
        description = "Read a PDF and analyze it with an LLM. Downloads the PDF (or reads a local file), extracts text via pdftotext, and sends it to the LLM endpoint (configured via DS4_API_BASE env var) with your prompt. Returns the LLM's analysis. For raw text extraction, use read_pdf instead."
    )]
    async fn analyze_pdf(
        &self,
        Parameters(p): Parameters<AnalyzePdfParams>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let max_chars = p.max_chars.unwrap_or(80000);

        // Resolve the source to a local file path (same logic as read_pdf).
        let is_url = p.url.starts_with("http://") || p.url.starts_with("https://");
        let (pdf_path, temp_to_clean): (std::path::PathBuf, Option<std::path::PathBuf>) = if is_url {
            let client = match reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .user_agent("mcp-pdf-server/0.1 (+https://arxiv.org)")
                .timeout(std::time::Duration::from_secs(60))
                .build()
            {
                Ok(c) => c,
                Err(e) => return Ok(tool_error(format!("http client build failed: {}", e))),
            };
            let resp = match client.get(&p.url).send().await {
                Ok(r) => r,
                Err(e) => return Ok(tool_error(format!("fetch failed: {}", e))),
            };
            if !resp.status().is_success() {
                return Ok(tool_error(format!("fetch returned HTTP {}", resp.status())));
            }
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => return Ok(tool_error(format!("read body failed: {}", e))),
            };
            let path = std::env::temp_dir().join(format!(
                "mcp-pdf-{}-{}.pdf",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            if let Err(e) = tokio::fs::write(&path, &bytes).await {
                return Ok(tool_error(format!("temp write failed: {}", e)));
            }
            (path.clone(), Some(path))
        } else {
            let path = std::path::PathBuf::from(&p.url);
            if !path.is_file() {
                return Ok(tool_error(format!("no such local file: {}", p.url)));
            }
            (path, None)
        };

        // Extract ALL text with pdftotext.
        let out = tokio::process::Command::new("pdftotext")
            .arg(&pdf_path)
            .arg("-")
            .output()
            .await;
        if let Some(tmp) = temp_to_clean {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        let out = match out {
            Ok(o) => o,
            Err(e) => return Ok(tool_error(format!("pdftotext spawn failed: {}", e))),
        };
        if !out.status.success() {
            return Ok(tool_error(format!(
                "pdftotext failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        // Truncate PDF text to max_chars.
        let pdf_text = String::from_utf8_lossy(&out.stdout);
        let pdf_text: String = pdf_text.chars().take(max_chars).collect();
        let total = pdf_text.chars().count();

        // Build LLM request.
        let llm_url = match std::env::var("DS4_API_BASE") {
            Ok(url) => format!("{}/v1/chat/completions", url.trim_end_matches('/')),
            Err(_) => return Ok(tool_error("DS4_API_BASE environment variable not set — configure it with the LLM endpoint URL".to_string())),
        };
        let model = std::env::var("DS4_MODEL").unwrap_or_else(|_| "default".to_string());

        let request = LlmChatRequest {
            model,
            messages: vec![
                LlmChatMessage {
                    role: "system".to_string(),
                    content: "You are a research analyst. Analyze the following paper or document thoroughly. Extract key insights, methods, results, and implications. Be precise and cite specific findings.".to_string(),
                },
                LlmChatMessage {
                    role: "user".to_string(),
                    content: format!("Here is the PDF text ({} chars):\n\n---\n{}\n---\n\nNow, {}", total, pdf_text, p.prompt),
                },
            ],
            stream: false,
            max_tokens: Some(4096),
        };

        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
        {
            Ok(c) => c,
            Err(e) => return Ok(tool_error(format!("http client build failed: {}", e))),
        };
        let resp = match client.post(&llm_url).json(&request).send().await {
            Ok(r) => r,
            Err(e) => return Ok(tool_error(format!("LLM call failed: {}", e))),
        };
        let response: LlmChatResponse = match resp.json().await {
            Ok(r) => r,
            Err(e) => return Ok(tool_error(format!("LLM response parse failed: {}", e))),
        };
        let reply = response.choices.first().map(|c| &c.message.content).unwrap_or(&"".to_string()).clone();

        Ok(tool_ok_text(reply))
    }

    #[tool(
        description = "Read a PDF and return its extracted text (via pdftotext). `url` accepts an http(s) URL (e.g. arxiv https://arxiv.org/pdf/<id>) OR a local file path. Returns a window [offset, offset+max_chars) of the full text plus the total length and the next offset — page through with `offset` to read ALL text, or pass a large `max_chars` to get it in one call."
    )]
    async fn read_pdf(
        &self,
        Parameters(p): Parameters<ReadPdfParams>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let offset = p.offset.unwrap_or(0);
        let max_chars = p.max_chars.unwrap_or(8000);

        // Resolve the source to a local file path: download a URL to a temp file,
        // or use a local path directly (no copy).
        let is_url = p.url.starts_with("http://") || p.url.starts_with("https://");
        let (pdf_path, temp_to_clean): (std::path::PathBuf, Option<std::path::PathBuf>) = if is_url {
            let client = match reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .user_agent("mcp-pdf-server/0.1 (+https://arxiv.org)")
                .timeout(std::time::Duration::from_secs(60))
                .build()
            {
                Ok(c) => c,
                Err(e) => return Ok(tool_error(format!("http client build failed: {}", e))),
            };
            let resp = match client.get(&p.url).send().await {
                Ok(r) => r,
                Err(e) => return Ok(tool_error(format!("fetch failed: {}", e))),
            };
            if !resp.status().is_success() {
                return Ok(tool_error(format!("fetch returned HTTP {}", resp.status())));
            }
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => return Ok(tool_error(format!("read body failed: {}", e))),
            };
            let path = std::env::temp_dir().join(format!(
                "mcp-pdf-{}-{}.pdf",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            if let Err(e) = tokio::fs::write(&path, &bytes).await {
                return Ok(tool_error(format!("temp write failed: {}", e)));
            }
            (path.clone(), Some(path))
        } else {
            let path = std::path::PathBuf::from(&p.url);
            if !path.is_file() {
                return Ok(tool_error(format!("no such local file: {}", p.url)));
            }
            (path, None)
        };

        // Extract ALL text with the host's pdftotext (output to stdout via "-").
        let out = tokio::process::Command::new("pdftotext")
            .arg(&pdf_path)
            .arg("-")
            .output()
            .await;
        if let Some(tmp) = temp_to_clean {
            let _ = tokio::fs::remove_file(&tmp).await;
        }

        let out = match out {
            Ok(o) => o,
            Err(e) => return Ok(tool_error(format!("pdftotext spawn failed: {}", e))),
        };
        if !out.status.success() {
            return Ok(tool_error(format!(
                "pdftotext failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }

        // Window the full text by [offset, offset+max_chars) so callers can page
        // through an arbitrarily long document without blowing the context.
        let text = String::from_utf8_lossy(&out.stdout);
        let total = text.chars().count();
        let shown: String = text.chars().skip(offset).take(max_chars).collect();
        let end = offset + shown.chars().count();
        let header = if end < total {
            format!(
                "[PDF text: {} chars total; showing [{}..{}). MORE REMAINS — call again with offset={} to continue.]\n\n",
                total, offset, end, end
            )
        } else if offset > 0 {
            format!(
                "[PDF text: {} chars total; showing [{}..{}) — END of document.]\n\n",
                total, offset, end
            )
        } else {
            format!("[PDF text: {} chars total (complete).]\n\n", total)
        };
        Ok(tool_ok_text(format!("{}{}", header, shown)))
    }
}

#[tool_handler]
impl ServerHandler for PdfServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "PDF reader: read_pdf(url) downloads a PDF and returns its text via pdftotext."
                    .to_string(),
            ),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("info"))
        .with_writer(std::io::stderr)
        .init();
    tracing::info!("Starting mcp-pdf-server");
    let server = PdfServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
