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

// ── Tag computation ───────────────────────────────────────────────

/// Compute a 4-char tag for a line of text. Uses a simple hash folded into
/// base62 to keep it short and token-efficient (antirez-style).
fn compute_tag(line: &str) -> String {
    let mut hash: u64 = 5381;
    for b in line.as_bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(*b as u64);
    }
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let h = hash % (CHARSET.len() as u64 * CHAR_LEN as u64 / 62); // fold
    let mut tag = String::new();
    let mut v = h;
    for _ in 0..4 {
        tag.push(CHARSET[(v % CHARSET.len() as u64) as usize] as char);
        v /= CHARSET.len() as u64;
    }
    tag.chars().rev().collect()
}

const CHAR_LEN: usize = 62;

/// Format a line with its number and tag: "12:rA3_ content here"
fn format_tagged_line(line_num: usize, content: &str) -> String {
    let tag = compute_tag(content);
    if content.is_empty() {
        format!("{line_num}:{tag}")
    } else {
        format!("{line_num}:{tag} {content}")
    }
}

/// Parse a tagged line spec "12:rA3_" or "12:rA3_ content". Returns (line, tag).
fn parse_tagged_spec(spec: &str) -> Option<(usize, String)> {
    let colon = spec.find(':')?;
    let line_num: usize = spec[..colon].parse().ok()?;
    // Tag is the 4 chars after colon
    let rest = &spec[colon + 1..];
    if rest.len() < 4 { return None; }
    Some((line_num, rest[..4].to_string()))
}

/// Verify that a line's content matches its tag.
fn verify_tag(content: &str, expected_tag: &str) -> bool {
    compute_tag(content) == expected_tag
}

// ── Parameters ───────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaggedReadParams {
    /// File path to read.
    file_path: String,
    /// Starting line number (1-based). Default: 1.
    #[serde(default)]
    start_line: Option<usize>,
    /// Number of lines to read. If omitted or 0, reads entire file.
    #[serde(default)]
    max_lines: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaggedSearchParams {
    /// Directory to search in (recursive).
    path: String,
    /// Regex pattern to match.
    pattern: String,
    /// File glob filter (e.g. "*.rs"). Default: all files.
    #[serde(default)]
    file_glob: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaggedEditParams {
    /// File path to edit.
    file_path: String,
    /// Line number to replace (1-based).
    line: usize,
    /// 4-char tag from the tagged_read output for CAS verification.
    tag: String,
    /// New content for this line. Use empty string to delete the line.
    new_content: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaggedEditRangeParams {
    /// File path to edit.
    file_path: String,
    /// Multi-line spec from tagged_read output: "11:rA3_\n12:Kq9z\n13:PX0b"
    lines_spec: String,
    /// New content replacing the specified line range. Use empty string to delete all lines.
    new_content: String,
}

// ── Server ───────────────────────────────────────────────────────

#[derive(Clone)]
struct TaggedFileopsServer {
    tool_router: ToolRouter<Self>,
}

impl TaggedFileopsServer {
    fn new() -> Self {
        Self { tool_router: Self::tool_router() }
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn read_file_lines(path: &str) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
    Ok(content.lines().map(String::from).collect())
}

#[tool_router]
impl TaggedFileopsServer {

    /// Read a file and return lines with tags for CAS-based editing.
    #[tool(
        description = "Read a file returning tagged lines (format: 'lineNum:tag content'). The tag is a 4-char checksum used by tagged_edit to verify the line hasn't changed. More token-efficient than reading raw content."
    )]
    async fn tagged_read(
        &self,
        Parameters(params): Parameters<TaggedReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let lines = try_tool!(read_file_lines(&params.file_path), "tagged_read failed");
        let start = params.start_line.unwrap_or(1).saturating_sub(1);
        let max = params.max_lines.unwrap_or(0);

        let end = if max == 0 { lines.len() } else { (start + max).min(lines.len()) };

        let mut out: Vec<String> = Vec::new();
        for i in start..end {
            // Line numbers are 1-based
            out.push(format_tagged_line(i + 1, &lines[i]));
        }

        if out.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                format!("{} (empty or no lines in range)", params.file_path),
            )]))
        } else {
            // Append file path header
            let mut output = format!("File: {}\n{}", params.file_path, out.join("\n"));
            if end < lines.len() {
                output.push_str(&format!("\n... ({} more lines)", lines.len() - end));
            }
            Ok(CallToolResult::success(vec![Content::text(output)]))
        }
    }

    /// Search files for a pattern and return tagged matching lines.
    #[tool(
        description = "Search recursively for a regex pattern in files. Returns matching lines with tags (format: 'file:lineNum:tag content'). Supports file glob filtering."
    )]
    async fn tagged_search(
        &self,
        Parameters(params): Parameters<TaggedSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let pattern = try_tool!(regex::Regex::new(&params.pattern), "invalid regex");
        // Walk the directory
        let mut results: Vec<String> = Vec::new();
        let walk_result = try_tool!(
            walk_files(&params.path),
            "tagged_search failed"
        );

        for file_path in &walk_result {
            if let Some(ref glob) = params.file_glob {
                if !file_path.ends_with(glob.trim_start_matches('*')) { continue; }
            }
            if let Ok(content) = std::fs::read_to_string(file_path) {
                for (i, line) in content.lines().enumerate() {
                    if pattern.is_match(line) {
                        results.push(format!(
                            "{}:{}",
                            file_path,
                            format_tagged_line(i + 1, line)
                        ));
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(CallToolResult::success(vec![Content::text(
                "No matches found".to_string(),
            )]))
        } else {
            let truncated = results.len();
            let display: String = results.into_iter().take(100).collect::<Vec<_>>().join("\n");
            let mut output = format!("Found {truncated} matches:\n{display}");
            if truncated > 100 {
                output.push_str(&format!("\n... (showing first 100 of {truncated})"));
            }
            Ok(CallToolResult::success(vec![Content::text(output)]))
        }
    }

    /// Edit a single line using CAS verification via tag.
    #[tool(
        description = "Replace one line in a file. Requires the line number and its 4-char tag (from tagged_read) for CAS verification — ensures the line hasn't changed since it was read. Pass empty new_content to delete the line."
    )]
    async fn tagged_edit(
        &self,
        Parameters(params): Parameters<TaggedEditParams>,
    ) -> Result<CallToolResult, McpError> {
        let lines = try_tool!(read_file_lines(&params.file_path), "tagged_read failed");

        if params.line == 0 || params.line > lines.len() {
            return Ok(tool_error(format!(
                "line {} out of range (file has {} lines)",
                params.line, lines.len()
            )));
        }

        let idx = params.line - 1;
        let current_content = &lines[idx];
        if !verify_tag(current_content, &params.tag) {
            return Ok(tool_error(format!(
                "CAS verification failed: line {} tag mismatch (file changed since read)",
                params.line
            )));
        }

        // Apply the edit
        let mut new_lines = lines.clone();
        if params.new_content.is_empty() {
            new_lines.remove(idx);
        } else {
            new_lines[idx] = params.new_content.clone();
        }
        try_tool!(write_file(&params.file_path, &new_lines), "tagged_edit failed");

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Edited line {} of {}", params.line, params.file_path
        ))]))
    }

    /// Edit a range of lines using CAS verification via tags.
    #[tool(
        description = "Replace multiple consecutive lines in a file. Requires the tagged spec from tagged_read (format: '11:rA3_\\n12:Kq9z') for CAS verification. Pass empty new_content to delete all specified lines."
    )]
    async fn tagged_edit_range(
        &self,
        Parameters(params): Parameters<TaggedEditRangeParams>,
    ) -> Result<CallToolResult, McpError> {
        let lines = try_tool!(read_file_lines(&params.file_path), "tagged_read failed");

        // Parse the multi-line spec
        let specs: Vec<&str> = params.lines_spec.split('\n').collect();
        if specs.is_empty() {
            return Ok(tool_error("lines_spec is empty"));
        }

        let first_spec = match parse_tagged_spec(specs[0]) {
            Some(v) => v,
            None => return Ok(tool_error("invalid lines_spec")),
        };
        let last_spec = match specs.last().and_then(|s| parse_tagged_spec(s)) {
            Some(v) => v,
            None => return Ok(tool_error("invalid lines_spec")),
        };

        if first_spec.0 > lines.len() || last_spec.0 < 1 || last_spec.0 > lines.len() {
            return Ok(tool_error("line range out of bounds"));
        }

        // Verify all tags
        for spec_str in &specs {
            let parsed = match parse_tagged_spec(spec_str) {
                Some(v) => v,
                None => return Ok(tool_error("invalid lines_spec entry")),
            };
            if parsed.0 == 0 || parsed.0 > lines.len() {
                return Ok(tool_error(format!("line {} out of range", parsed.0)));
            }
            if !verify_tag(&lines[parsed.0 - 1], &parsed.1) {
                return Ok(tool_error(format!(
                    "CAS verification failed: line {} tag mismatch",
                    parsed.0
                )));
            }
        }

        // Apply the edit — replace lines first..last with new_content (possibly multiple lines)
        let start_idx = first_spec.0 - 1;
        let end_count = last_spec.0 - first_spec.0 + 1;

        let mut new_lines = lines.clone();
        for _ in 0..end_count {
            new_lines.remove(start_idx);
        }

        if !params.new_content.is_empty() {
            let replacement: Vec<String> = params
                .new_content
                .lines()
                .map(String::from)
                .collect();
            for (i, line) in replacement.into_iter().enumerate() {
                new_lines.insert(start_idx + i, line);
            }
        }

        try_tool!(write_file(&params.file_path, &new_lines), "tagged_edit_range failed");

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Edited lines {}-{} of {}",
            first_spec.0,
            last_spec.0,
            params.file_path
        ))]))
    }
}

// ── File walking helper ───────────────────────────────────────────

fn walk_files(root: &str) -> Result<Vec<String>> {
    let mut files = Vec::new();
    walk_dir(std::path::PathBuf::from(root), &mut files)?;
    Ok(files)
}

fn walk_dir(dir: std::path::PathBuf, files: &mut Vec<String>) -> Result<()> {
    if !dir.is_dir() { return Ok(()); }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        // Skip hidden dirs and common noise
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" { continue; }
        if path.is_dir() {
            walk_dir(path, files)?;
        } else {
            files.push(path.to_string_lossy().to_string());
        }
    }
    Ok(())
}

fn write_file(path: &str, lines: &[String]) -> Result<()> {
    let content = lines.join("\n") + "\n";
    std::fs::write(path, content)
        .map_err(|e| anyhow::anyhow!("writing {path}: {e}"))?;
    Ok(())
}

// ── ServerHandler ─────────────────────────────────────────────────

#[tool_handler]
impl ServerHandler for TaggedFileopsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "MCP server providing tag-based file operations (read, search, edit). \
                 Lines are returned with 4-char checksum tags for CAS verification during edits. \
                 More token-efficient than verbatim old_content in standard edit_file."
                    .to_string(),
            ),
        }
    }
}

// ── Main ──────────────────────────────────────────────────────────

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("Starting mcp-tagged-fileops server");
    let server = TaggedFileopsServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_4_chars() {
        assert_eq!(compute_tag("hello world").len(), 4);
        assert_eq!(compute_tag("").len(), 4);
        assert_eq!(compute_tag("fn main() {}").len(), 4);
    }

    #[test]
    fn tag_deterministic() {
        let t1 = compute_tag("same line");
        let t2 = compute_tag("same line");
        assert_eq!(t1, t2);
    }

    #[test]
    fn tag_differs_for_different_content() {
        let t1 = compute_tag("line one");
        let t2 = compute_tag("line two");
        assert_ne!(t1, t2);
    }

    #[test]
    fn verify_tag_roundtrip() {
        let content = "int count = 10;";
        let tag = compute_tag(content);
        assert!(verify_tag(content, &tag));
        assert!(!verify_tag("different", &tag));
    }

    #[test]
    fn format_and_parse_tagged_line() {
        let formatted = format_tagged_line(12, "if (count > limit) {");
        assert!(formatted.starts_with("12:"));
        // Tag is the 4 chars after colon
        let rest = &formatted[3..]; // skip "12:"
        assert_eq!(&rest[..4].len(), &4);
    }

    #[test]
    fn parse_tagged_spec_basic() {
        let spec = "10:rA3_";
        let (line, tag) = parse_tagged_spec(spec).unwrap();
        assert_eq!(line, 10);
        assert_eq!(tag.len(), 4);
    }
}
