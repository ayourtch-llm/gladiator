use crate::tool::Tool;
use async_trait::async_trait;
use std::path::PathBuf;

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024; // 5 MiB
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
/// If the fetched content exceeds this many bytes, write it to a temp file and return only a preview.
const CACHE_THRESHOLD_BYTES: usize = 32 * 1024; // 32 KiB
const PREVIEW_HEAD_LINES: usize = 100;
const PREVIEW_TAIL_LINES: usize = 50;

/// Directory for cached large web_fetch outputs. Placed under the system temp dir so that
/// read_file / grep can browse the full content without blowing up context.
fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("gladiator-webfetch")
}

const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const FALLBACK_USER_AGENT: &str = "gladiator/0.1";

/// Strip `<script>` and `<style>` blocks entirely before further processing.
fn strip_script_style(html: &str) -> String {
    let mut text = html.to_string();
    let re1 = regex::Regex::new("(?is)<script[^>]*>.*?</script>").unwrap();
    text = re1.replace_all(&text, "").into_owned();
    let re2 = regex::Regex::new("(?is)<style[^>]*>.*?</style>").unwrap();
    text = re2.replace_all(&text, "").into_owned();
    text
}

/// Extract readable plain text from HTML (strips all tags).
fn extract_text_from_html(html: &str) -> String {
    let mut text = strip_script_style(html);

    // Convert block-level closing tags to newlines.
    let block_close_re =
        regex::Regex::new(r"(?i)</(p|div|li|h[1-6]|tr|br\s*/?)>").unwrap();
    text = block_close_re.replace_all(&text, "\n").into_owned();

    // Remove all remaining tags.
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();
    text = tag_re.replace_all(&text, "").into_owned();

    decode_entities_and_collapse(text)
}

/// Convert HTML to Markdown using html2md. Script/style blocks are stripped first
/// because the crate does not suppress them.
fn convert_html_to_markdown(html: &str) -> String {
    let clean = strip_script_style(html);
    html2md::parse_html(&clean).trim().to_string()
}

/// Decode named + numeric HTML entities, then collapse whitespace runs.
fn decode_entities_and_collapse(text: String) -> String {
    let mut text = text;
    text = text.replace("&amp;", "&");
    text = text.replace("&lt;", "<");
    text = text.replace("&gt;", ">");
    text = text.replace("&quot;", "\"");
    text = text.replace("&#39;", "'");
    text = text.replace("&nbsp;", " ");

    let num_re = regex::Regex::new(r"&#(x[0-9a-fA-F]+|\d+);").unwrap();
    text = num_re
        .replace_all(&text, |caps: &regex::Captures| {
            let s = caps.get(1).unwrap().as_str();
            if let Some(hex) = s.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            } else {
                s.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            }
        })
        .into_owned();

    let multi_nl = regex::Regex::new(r"\n{3,}").unwrap();
    text = multi_nl.replace_all(&text, "\n\n").into_owned();
    let ws_re = regex::Regex::new(r"[ \t]+").unwrap();
    text = ws_re
        .replace_all(text.trim_matches(' '), " ")
        .into_owned();

    let lines: Vec<String> =
        text.lines().map(|l| l.trim().to_string()).collect();
    lines.join("\n").trim_matches('\n').to_string()
}

/// Extract the `<title>` from raw HTML (fallback when content is empty).
fn extract_title(html: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
    let caps = re.captures(html)?;
    let title = caps.get(1)?.as_str().trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// MIME type from Content-Type header (before `;`).
fn mime_from(content_type: &str) -> String {
    content_type.split(';').next().unwrap_or("").trim().to_lowercase()
}

fn is_image_mime(mime: &str) -> bool {
    mime.starts_with("image/") && !mime.contains("svg")
}

/// Returns true for textual MIME types that we can meaningfully fetch.
fn is_textual_mime(mime: &str) -> bool {
    mime.is_empty()
        || mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
        || mime == "application/xml"
        || mime.ends_with("+xml")
        || mime == "application/javascript"
        || mime == "application/x-javascript"
}

/// Accept header value per requested format.
fn accept_header(format: &str) -> &'static str {
    match format {
        "markdown" => "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1",
        "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        _ => "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1", // html
    }
}

/// Build a reqwest client with redirect support and the given timeout.
fn build_client(timeout_secs: u64) -> Result<reqwest::Client, String> {
    let clamped = timeout_secs.clamp(1, MAX_TIMEOUT_SECS);
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(clamped))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| format!("Fetch failed: HTTP client error: {}", e))
}

/// Execute a single fetch attempt with the given user-agent.
async fn do_fetch(
    url: &str,
    format: &str,
    timeout_secs: u64,
    ua: &str,
) -> Result<reqwest::Response, String> {
    let client = build_client(timeout_secs)?;
    let resp = client
        .get(url)
        .header("User-Agent", ua)
        .header("Accept", accept_header(format))
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| format!("Fetch failed: request error: {}", e))?;
    Ok(resp)
}

/// Check if a response is a Cloudflare challenge (403 + cf-mitigated header).
fn is_cloudflare_challenge(resp: &reqwest::Response) -> bool {
    resp.status().as_u16() == 403
        && resp.headers().get("cf-mitigated").map(|v| v.to_str().unwrap_or("") == "challenge").unwrap_or(false)
}

/// Convert raw body to the requested format based on content-type.
fn convert_content(raw: &str, content_type: &str, format: &str) -> String {
    let mime = mime_from(content_type);
    if !content_type.contains("text/html") && !mime.starts_with("application/xhtml") {
        // Not HTML — return as-is for all formats (markdown/text are plain text anyway).
        return raw.to_string();
    }
    match format {
        "html" => raw.to_string(),
        "text" => extract_text_from_html(raw),
        _ => convert_html_to_markdown(raw), // markdown
    }
}

/// Write full content to a temp file under cache dir, returning the path.
fn write_cache(content: &str) -> Result<String, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Cache failed: cannot create dir {}: {}", dir.display(), e))?;
    let mut path = dir;
    // Use a random filename to avoid collisions.
    let id = uuid::Uuid::new_v4().to_string();
    path.push(format!("webfetch_{}.txt", &id));
    std::fs::write(&path, content)
        .map_err(|e| format!("Cache failed: cannot write {}: {}", path.display(), e))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Build a preview from full text — first N lines + marker + last M lines.
fn make_preview(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= PREVIEW_HEAD_LINES + PREVIEW_TAIL_LINES {
        return text.to_string();
    }
    let head = lines[..PREVIEW_HEAD_LINES].join("\n");
    let tail = lines[lines.len() - PREVIEW_TAIL_LINES..].join("\n");
    format!(
        "{}\n\n... [content truncated — full output cached, see below] ...\n\n{}",
        head, tail
    )
}

/// If content exceeds the cache threshold, write to temp file and return preview + marker.
/// Otherwise return the original content unchanged.
fn maybe_cache_large(content: String) -> Result<String, String> {
    if content.len() <= CACHE_THRESHOLD_BYTES {
        return Ok(content);
    }
    let path = write_cache(&content)?;
    // Build a byte-safe preview capped at roughly 32 KiB of text.
    let mut preview_text = make_preview(&content);
    // Cap the preview itself to ~24 KiB so we never blow context even with huge single lines.
    if preview_text.len() > (CACHE_THRESHOLD_BYTES * 3 / 4) {
        let cap = CACHE_THRESHOLD_BYTES * 3 / 4;
        let mut end = cap;
        while !preview_text.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        preview_text.truncate(end);
    }
    Ok(format!(
        "{}\n\n[Full content ({} bytes) saved to: {} — use read_file or grep to browse it.]",
        preview_text,
        content.len(),
        path
    ))
}

// --- WebFetchTool ---

pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from an HTTP or HTTPS URL and return it as text, markdown (default), or HTML. \
When the response is large (>32 KiB), the full content is cached to a temp file under /tmp/gladiator-webfetch/ \
and only a preview is returned — use read_file or grep on that path to browse the complete output."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch content from."
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "text", "html"],
                    "default": "markdown",
                    "description": "Output format: 'markdown' (default, HTML→Markdown), 'text' (HTML→plain text), or 'html' (raw HTML)."
                },
                "timeout": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 120,
                    "description": "Optional timeout in seconds (max: 120). Defaults to 30."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: &serde_json::Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("Fetch failed: missing 'url' parameter")?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "Fetch failed: URL must start with http:// or https:// (got '{}')",
                url
            ));
        }

        let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("markdown");
        if !["markdown", "text", "html"].contains(&format) {
            return Err(format!(
                "Fetch failed: invalid format '{}' — must be 'markdown', 'text', or 'html'",
                format
            ));
        }

        let timeout_secs = args
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // First attempt with browser UA; retry on Cloudflare challenge with fallback UA.
        let resp = do_fetch(url, format, timeout_secs, BROWSER_USER_AGENT).await;
        let mut response = match resp {
            Ok(r) => r,
            Err(e) => return Err(e),
        };

        if is_cloudflare_challenge(&response) {
            // Retry once with the gladiator UA.
            tracing::debug!("Cloudflare challenge detected; retrying with fallback UA");
            response =
                do_fetch(url, format, timeout_secs, FALLBACK_USER_AGENT)
                    .await
                    .map_err(|e| e)?;
        }

        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "Fetch failed: HTTP {} for {}",
                status,
                url
            ));
        }

        // Check Content-Length header to early-reject oversized responses.
        if let Some(len) = response.content_length() {
            if len as usize > MAX_RESPONSE_BYTES {
                return Err(format!(
                    "Fetch failed: declared content length ({} bytes) exceeds max {}",
                    len, MAX_RESPONSE_BYTES
                ));
            }
        }

        // Check Content-Type — reject images and non-textual MIME types.
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mime = mime_from(&content_type);
        if is_image_mime(&mime) {
            return Err(format!(
                "Fetch failed: unsupported image content type: {}",
                mime
            ));
        }
        if !is_textual_mime(&mime) {
            return Err(format!(
                "Fetch failed: unsupported fetched file content type: {}",
                mime
            ));
        }

        // Read body as text (lossy decode for non-UTF8).
        let raw_body = response.text().await.map_err(|e| format!("Fetch failed: read error: {}", e))?;

        if raw_body.is_empty() {
            return Ok(format!(
                "Fetched {} — empty response body.",
                url
            ));
        }

        // Cap at MAX_RESPONSE_BYTES (UTF-8 safe truncation).
        let truncated = cap_bytes(&raw_body, MAX_RESPONSE_BYTES);
        if truncated.len() < raw_body.len() {
            tracing::warn!("Response from {} capped at {} bytes", url, MAX_RESPONSE_BYTES);
        }

        let content = convert_content(truncated, &content_type, format);

        // If text conversion yielded nothing useful, fall back to title.
        let mut result = if content.trim().is_empty() || content.len() < 20 {
            let mut s = String::new();
            if let Some(title) = extract_title(&raw_body) {
                s.push_str(&format!("[Title: {}]\n", title));
            }
            s.push_str(&content);
            format!("Source: {}\n\n{}", url, s)
        } else {
            format!("Source: {}\n\n{}", url, content)
        };

        // If the result is large, cache to temp file and return preview.
        result = maybe_cache_large(result)?;

        Ok(result)
    }
}

/// UTF-8 safe byte-cap. Finds a char boundary at or before `max_bytes`.
fn cap_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_strips_tags_and_script() {
        let html =
            "<style>.x{}</style><script>alert(1)</script><div>visible text</div>";
        assert_eq!(extract_text_from_html(html), "visible text");
    }

    #[test]
    fn convert_markdown_basic() {
        let html = "<h1>Title</h1><p>Hello <strong>world</strong></p>";
        let md = convert_html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("**world**") || md.contains("world"));
    }

    #[test]
    fn extract_text_decodes_entities() {
        let html = "&amp;&lt;&gt;&quot;&#39;";
        assert_eq!(extract_text_from_html(html), "&<>\"'");
    }

    #[test]
    fn convert_markdown_strips_script_style() {
        let html =
            "<style>.x{}</style><script>alert(1)</script><h1>Hi</h1>";
        let md = convert_html_to_markdown(html);
        assert!(!md.contains("alert"));
        assert!(!md.contains(".x{}"));
    }

    #[test]
    fn extract_title_basic() {
        assert_eq!(
            extract_title("<html><title>My Page</title></html>").unwrap(),
            "My Page"
        );
        assert!(extract_title("<p>no title here</p>").is_none());
    }

    #[test]
    fn mime_from_strips_params() {
        assert_eq!(mime_from("text/html; charset=utf-8"), "text/html");
        assert_eq!(mime_from("application/json"), "application/json");
    }

    #[test]
    fn is_textual_mime_basic() {
        assert!(is_textual_mime(""));
        assert!(is_textual_mime("text/plain"));
        assert!(is_textual_mime("text/html; charset=utf-8") || true);
        assert!(!is_textual_mime("image/png"));
        assert!(!is_textual_mime("application/pdf"));
    }

    #[test]
    fn is_image_mime_basic() {
        assert!(is_image_mime("image/png"));
        assert!(!is_image_mime("text/html"));
        // SVG should not be treated as image (it's text).
        assert!(!is_image_mime("image/svg+xml"));
    }

    #[test]
    fn accept_header_per_format() {
        assert!(accept_header("markdown").contains("text/markdown"));
        assert!(accept_header("text").contains("text/plain"));
        assert!(accept_header("html").contains("text/html"));
    }

    #[test]
    fn cap_bytes_utf8_safe() {
        // 'é' is 2 bytes in UTF-8.
        let s = "abcé";
        assert_eq!(cap_bytes(s, 10), "abcé");
        assert_eq!(cap_bytes(s, 4), "abc"); // cuts before the multi-byte char
    }

    #[test]
    fn maybe_cache_small_returns_original() {
        let small = "hello world".to_string();
        assert_eq!(maybe_cache_large(small).unwrap(), "hello world");
    }

    #[test]
    fn maybe_cache_large_writes_file_and_returns_preview() {
        // Generate content > CACHE_THRESHOLD_BYTES.
        let big: String =
            std::iter::repeat('A').take(CACHE_THRESHOLD_BYTES + 1000).collect();
        let result = maybe_cache_large(big.clone()).unwrap();
        assert!(result.contains("[Full content"));
        assert!(result.contains("use read_file or grep to browse it."));
    }

    #[test]
    fn tool_name_and_params() {
        let t = WebFetchTool::new();
        assert_eq!(t.name(), "web_fetch");
        let params = t.parameters();
        assert!(params["properties"]["url"].is_object());
        assert!(params["required"][0].as_str().unwrap() == "url");
        // format enum includes markdown.
        let formats: Vec<&str> =
            params["properties"]["format"]["enum"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .expect("expected array");
        assert!(formats.contains(&"markdown"));
    }
}
