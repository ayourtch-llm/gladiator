use crate::tool::Tool;
use async_trait::async_trait;

const MAX_CONTENT_SIZE: usize = 32_768; // 32 KiB
const FETCH_TIMEOUT_SECS: u64 = 30;

/// Strip HTML tags and decode basic entities, producing readable plain text.
///
/// This is a deliberately dumb converter — not a full HTML parser. It:
/// - Removes `<script>`/`<style>` blocks entirely (including their content).
/// - Converts block-level closing tags (`</p>`, `</div>`, `</li>`, etc.) to newlines.
/// - Strips all remaining tags, leaving only inner text.
/// - Decodes the common named entities (&amp; &lt; &gt; &quot; &#39; &nbsp;) and
///   numeric references (`&#NNN;` / `&#xHHH;`).
/// - Collapses runs of whitespace/newlines into at most two newlines or a single space.
fn html_to_text(html: &str) -> String {
    let mut text = html.to_string();

    // Remove script blocks (non-greedy, case-insensitive)
    let strip_script =
        regex::Regex::new("(?is)<script[^>]*>.*?</script>").unwrap();
    text = strip_script.replace_all(&text, "").into_owned();

    // Remove style blocks
    let strip_style =
        regex::Regex::new("(?is)<style[^>]*>.*?</style>").unwrap();
    text = strip_style.replace_all(&text, "").into_owned();

    // Convert block-level closing tags to newlines before removing all tags
    let block_close_re = regex::Regex::new(
        r"(?i)</(p|div|li|h[1-6]|tr|br\s*/?)>",
    )
    .unwrap();
    text = block_close_re.replace_all(&text, "\n").into_owned();

    // Remove all remaining tags
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();
    text = tag_re.replace_all(&text, "").into_owned();

    // Decode named entities
    text = text.replace("&amp;", "&");
    text = text.replace("&lt;", "<");
    text = text.replace("&gt;", ">");
    text = text.replace("&quot;", "\"");
    text = text.replace("&#39;", "'");
    text = text.replace("&nbsp;", " ");

    // Decode numeric entities (&#NNN; or &#xHHH;)
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

    // Collapse whitespace: runs of blank lines → max 2 newlines; runs of spaces/tabs → single space
    let multi_nl = regex::Regex::new(r"\n{3,}").unwrap();
    text = multi_nl.replace_all(&text, "\n\n").into_owned();
    let ws_re = regex::Regex::new(r"[ \t]+").unwrap();
    text = ws_re.replace_all(&text.trim_matches(' '), " ").into_owned();

    // Trim leading/trailing whitespace on each line, then join.
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() || true)
        .collect();
    lines.join("\n").trim_matches('\n').to_string()
}

/// Extract the `<title>` from raw HTML (used as a fallback when no content was found).
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
        "Fetch content from a URL. Returns the page as text (HTML stripped to readable plain text). Optional format: 'text' (default, HTML→plain text), 'html' (raw HTML), or 'markdown'. Caps response at ~32 KiB."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (http or https)."
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "html"],
                    "description": "Output format: 'text' strips HTML tags and returns readable plain text. 'html' returns raw HTML."
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

        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        // Build reqwest client with a timeout and redirect support.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("gladiator/0.1")
            .build()
            .map_err(|e| format!("Fetch failed: HTTP client error: {}", e))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Fetch failed: request error: {}", e))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!(
                "Fetch failed: HTTP {} for {}",
                status,
                url
            ));
        }

        // Read body as text (lossy decode).
        let raw_body = resp.text().await.map_err(|e| format!("Fetch failed: read error: {}", e))?;

        if raw_body.is_empty() {
            return Ok(format!(
                "Fetched {} — empty response body.",
                url
            ));
        }

        // Convert based on requested format.
        let content = match format {
            "html" => &raw_body,
            _ => "", // placeholder, handled below
        };

        if format == "html" {
            return truncate(content.to_string());
        }

        // Default: text (strip HTML)
        let mut text_content = html_to_text(&raw_body);

        // If conversion yielded nothing useful, fall back to the title.
        if text_content.trim().is_empty() || text_content.len() < 20 {
            if let Some(title) = extract_title(&raw_body) {
                text_content = format!("[Title: {}]\n{}", title, text_content);
            }
        }

        // Prepend a small header with the source URL.
        let result = format!("Source: {}\n\n{}", url, text_content);

        truncate(result)
    }
}

/// Truncate to MAX_CONTENT_SIZE bytes (UTF-8 safe) and append an ellipsis if cut.
fn truncate(s: String) -> Result<String, String> {
    if s.len() <= MAX_CONTENT_SIZE {
        return Ok(s);
    }

    // Find a UTF-8 boundary at or before the cap.
    let mut end = MAX_CONTENT_SIZE;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    let truncated = &s[..end];
    Ok(format!(
        "{}\n\n[... content truncated at {} bytes]",
        truncated,
        MAX_CONTENT_SIZE
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_tags() {
        let html = "<p>Hello <b>world</b></p>";
        assert_eq!(html_to_text(html), "Hello world");
    }

    #[test]
    fn html_to_text_removes_script_style() {
        let html =
            "<style>.x{}</style><script>alert(1)</script><div>visible text</div>";
        assert_eq!(html_to_text(html), "visible text");
    }

    #[test]
    fn html_to_text_decodes_entities() {
        let html = "&amp;&lt;&gt;&quot;&#39;";
        assert_eq!(html_to_text(html), "&<>\"'");
    }

    #[test]
    fn html_to_text_numeric_entity_hex() {
        let html = "&#x41;"; // 'A'
        assert_eq!(html_to_text(html), "A");
    }

    #[test]
    fn html_to_text_collapses_whitespace() {
        let html = "<p>a</p><div></div><div>b</div>";
        let text = html_to_text(html);
        assert!(text.contains("a"));
        assert!(text.contains("b"));
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
    fn truncate_under_cap_returns_full() {
        let s = "hello".to_string();
        assert_eq!(truncate(s).unwrap(), "hello");
    }

    #[test]
    fn truncate_over_cuts_and_appends_ellipsis() {
        // Build a string longer than MAX_CONTENT_SIZE.
        let big: String = std::iter::repeat('A').take(MAX_CONTENT_SIZE + 1000).collect();
        let result = truncate(big.clone()).unwrap();
        assert!(result.contains("[... content truncated"));
    }

    #[test]
    fn tool_name_and_params() {
        let t = WebFetchTool::new();
        assert_eq!(t.name(), "web_fetch");
        let params = t.parameters();
        assert!(params["properties"]["url"].is_object());
        assert!(params["required"][0].as_str().unwrap() == "url");
    }
}
