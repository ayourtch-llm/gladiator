//! Render `edit_file` / `plan_edits` tool-call arguments as a unified-diff
//! so the TUI shows what changed instead of raw JSON. Backed by the `similar`
//! crate's Myers diff algorithm.

use similar::TextDiff;

/// Build a unified-diff string between two texts, with file headers.
fn unified_diff(old: &str, new: &str, old_header: &str, new_header: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("--- {}", old_header), &format!("+++ {}", new_header))
        .to_string()
}

/// Render a diff for an `edit_file` tool call.
pub fn render_edit_file_diff(args: &serde_json::Value) -> Option<String> {
    let file_path = args.get("file_path").and_then(|v| v.as_str())?;
    let old_content = args.get("old_content").and_then(|v| v.as_str()).unwrap_or("");
    let new_content = args.get("new_content").and_then(|v| v.as_str()).unwrap_or("");

    if old_content == new_content {
        return None;
    }

    Some(unified_diff(old_content, new_content, file_path, file_path))
}

/// Render a diff for an `apply_edits` / `plan_edits` tool call.
pub fn render_apply_edits_diff(args: &serde_json::Value) -> Option<String> {
    let edits = args.get("edits").and_then(|v| v.as_array())?;
    if edits.is_empty() {
        return None;
    }

    let mut out: Vec<String> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let file_path = match edit.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => continue,
        };
        let old_content = edit.get("old_content").and_then(|v| v.as_str()).unwrap_or("");
        let new_content = edit.get("new_content").and_then(|v| v.as_str()).unwrap_or("");

        if i > 0 {
            out.push(String::new());
        }

        if let Some(desc) = edit.get("description").and_then(|v| v.as_str()) {
            if !desc.is_empty() {
                out.push(format!("# {}", desc));
            }
        }

        if old_content == new_content {
            // No change for this edit — show a no-op marker
            out.push(format!("--- {} (edit {}, no changes)", file_path, i + 1));
            out.push(format!("+++ {} (edit {}, no changes)", file_path, i + 1));
            continue;
        }

        let header = format!("{} (edit {})", file_path, i + 1);
        let diff = unified_diff(old_content, new_content, &header, &header);
        out.push(diff);
    }

    if out.is_empty() {
        return None;
    }
    Some(out.join("\n"))
}

/// Render a command-execution tool-call's arguments as a shell-prompt line.
/// Returns `None` for non-command tools or empty commands.
pub fn render_tool_call(name: &str, args: &serde_json::Value) -> Option<String> {
    let cmd = match name {
        "bash" | "run_command" => args.get("command").and_then(|v| v.as_str()),
        _ => None,
    }?;
    if !cmd.is_empty() {
        Some(format!("$ {}", cmd))
    } else {
        None
    }
}

/// Top-level entry point: given the tool name and its parsed JSON arguments,
/// produce a diff string when applicable. Returns `None` for non-diff tools
/// or when there's nothing to show.
pub fn render_tool_diff(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    match tool_name {
        "edit_file" => render_edit_file_diff(args),
        "apply_edits" | "plan_edits" => render_apply_edits_diff(args),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(old: &str, new: &str) -> Option<String> {
        if old == new { return None; }
        Some(unified_diff(old, new, "/x", "/x"))
    }

    #[test]
    fn no_change_returns_none() {
        assert!(diff("hello\nworld", "hello\nworld").is_none());
    }

    #[test]
    fn single_line_replace_produces_removed_and_added_lines() {
        let d = diff("line one\nold line", "line one\nnew content")
            .expect("should produce a diff");
        // similar's unified_diff with header includes the --- / +++ lines
        assert!(d.contains("--- /x"));
        assert!(d.contains("+++ /x"));
        assert!(d.contains("@@ "));
        assert!(d.contains("-old line") || d.contains("-old line\n\\ No newline at end of file"));
        assert!(d.contains("+new content") || d.contains("+new content\n\\ No newline at end of file"));
    }

    #[test]
    fn insert_at_start() {
        let d = diff("b", "a\nb").expect("should produce a diff");
        assert!(d.contains("+a"));
    }

    #[test]
    fn delete_at_end_produces_removed_line() {
        let d = diff("a\nb", "a").expect("should produce a diff");
        assert!(d.contains("-b") || d.contains("-b\n\\ No newline at end of file"));
    }

    #[test]
    fn edit_file_diff_includes_filename_headers() {
        let args = serde_json::json!({
            "file_path": "/tmp/foo.rs",
            "old_content": "fn old() {}",
            "new_content": "fn new() {}"
        });
        let d = render_edit_file_diff(&args).expect("should produce a diff");
        assert!(d.contains("--- /tmp/foo.rs"));
        assert!(d.contains("+++ /tmp/foo.rs"));
        assert!(d.contains("-fn old() {}") || d.contains("\\ No newline at end of file"));
        assert!(d.contains("+fn new() {}") || d.contains("\\ No newline at end of file"));
    }

    #[test]
    fn edit_file_missing_fields_returns_none() {
        // Both empty → no change
        assert_eq!(
            render_edit_file_diff(&serde_json::json!({
                "file_path": "/x",
                "old_content": "",
                "new_content": ""
            })),
            None,
        );
        // Missing file_path entirely → None
        let args = serde_json::json!({"old_content": "a", "new_content": "b"});
        assert_eq!(render_edit_file_diff(&args), None);
    }

    #[test]
    fn apply_edits_renders_multiple_hunks() {
        let args = serde_json::json!({
            "edits": [
                {"file_path": "/a.rs", "description": "first edit", "old_content": "x", "new_content": "y"},
                {"file_path": "/b.rs", "old_content": "p\nq", "new_content": "r"}
            ]
        });
        let d = render_apply_edits_diff(&args).expect("should produce a multi-edit diff");
        assert!(d.contains("# first edit"));
        // First edit header
        assert!(d.contains("--- /a.rs (edit 1)"));
        assert!(d.contains("-x") || d.contains("\\ No newline at end of file"));
    }

    #[test]
    fn unknown_tool_returns_none() {
        let args = serde_json::json!({"file_path": "/x"});
        assert_eq!(render_tool_diff("bash", &args), None);
    }

    #[test]
    fn render_bash_command_basic() {
        let args = serde_json::json!({"command": "ls -la"});
        assert_eq!(
            render_tool_call("bash", &args),
            Some("$ ls -la".to_string())
        );
    }

    #[test]
    fn render_run_command_basic() {
        let args = serde_json::json!({"command": "echo hi"});
        assert_eq!(
            render_tool_call("run_command", &args),
            Some("$ echo hi".to_string())
        );
    }

    #[test]
    fn render_non_command_tool_returns_none() {
        let args = serde_json::json!({"file_path": "/x"});
        assert!(render_tool_call("edit_file", &args).is_none());
    }

    #[test]
    fn render_empty_command_returns_none() {
        let args = serde_json::json!({"command": ""});
        assert!(render_tool_call("bash", &args).is_none());
    }
}
