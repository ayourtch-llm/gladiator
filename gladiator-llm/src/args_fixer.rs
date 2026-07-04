//! Validates and, when possible, repairs the `arguments` JSON string carried by
//! streamed tool calls.
//!
//! Tool call arguments arrive as a concatenation of incremental string deltas
//! (see `openai_chat::parse_event`). A dropped or delayed SSE chunk can leave
//! the accumulated string as truncated / partly-escaped JSON — e.g.
//! `{"command":"echo hi"` (no closing brace) or `{"command":"echo \"hi` (EOF
//! while parsing a string). Rather than forward known-broken payloads to the
//! agent (which has no way to recover the missing bytes), we attempt a small
//! set of conservative, heuristic repairs here at the source. Anything we
//! cannot repair is forwarded unchanged so the downstream parser still surfaces
//! a faithful error.

use serde_json::Value;

/// Validate (and try to repair) the `arguments` string of a single tool call.
///
/// On success the returned string is guaranteed to parse as JSON; if every
/// repair strategy fails the original input is returned unchanged so callers
/// can still observe the underlying parse failure.
pub fn fix_args_string(args: &str) -> String {
    // Fast path: already valid.
    if serde_json::from_str::<Value>(args).is_ok() {
        return args.to_string();
    }

    for candidate in repair_candidates(args) {
        if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
            // Re-serialize so downstream sees canonical formatting.
            return v.to_string();
        }
    }

    args.to_string()
}

/// Repair the `function.arguments` field of a single tool-call JSON value
/// in place. Returns `true` if the field was modified.
pub fn repair_tool_call(tc: &mut Value) -> bool {
    let original = match tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()) {
        Some(s) => s.to_string(),
        None => return false,
    };
    let fixed = fix_args_string(&original);
    if fixed != original {
        tc["function"]["arguments"] = Value::String(fixed);
        true
    } else {
        false
    }
}

/// Repair a batch of tool-call JSON values. Returns how many were modified.
pub fn repair_tool_calls(tool_calls: &mut [Value]) -> usize {
    tool_calls.iter_mut().map(repair_tool_call).filter(|&b| b).count()
}

/// Produce ordered repair candidates, cheapest/most-likely first.
fn repair_candidates(s: &str) -> Vec<String> {
    let mut out = Vec::new();

    // 1) Escape bare control characters inside string literals (a common
    //    streaming artifact where a literal newline leaks through).
    let escaped = escape_string_control_chars(s);
    if escaped != s {
        out.push(escaped.clone());
    }

    // 2) Balance braces/brackets/quotes that were left open by a truncated
    //    stream (the "EOF while parsing string" class of failure).
    out.push(append_missing_closers(&escaped));

    // 3) Truncate at progressively earlier `,` boundaries and rebalance, to
    //    recover when the tail is unrecoverable (e.g. truncated mid-key).
    out.extend(truncate_at_commas(&escaped));

    out
}

/// Walk the string respecting JSON string/escape state and append the minimal
/// set of closers needed to balance `{`, `[`, and `"` at EOF.
fn append_missing_closers(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escape = false;

    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' | '[' => stack.push(ch),
                '}' | ']' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    let mut result = String::from(s);

    // Trailing lone backslash (truncated mid-escape): drop it so we can close
    // the string cleanly.
    if escape {
        result.pop();
    }

    if in_string {
        result.push('"');
    }

    for opener in stack.into_iter().rev() {
        match opener {
            '{' => result.push('}'),
            '[' => result.push(']'),
            _ => {}
        }
    }

    result
}

/// Escape unescaped control characters (newline, tab, etc.) that appear inside
/// JSON string literals. Leaves bytes outside string literals untouched.
fn escape_string_control_chars(s: &str) -> String {
    let mut in_string = false;
    let mut escape = false;
    let mut out = String::with_capacity(s.len());

    for ch in s.chars() {
        if in_string {
            if escape {
                escape = false;
                out.push(ch);
                continue;
            }
            match ch {
                '\\' => {
                    escape = true;
                    out.push(ch);
                }
                '"' => {
                    in_string = false;
                    out.push(ch);
                }
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                _ => out.push(ch),
            }
        } else {
            match ch {
                '"' => in_string = true,
                _ => {}
            }
            out.push(ch);
        }
    }

    out
}

/// Try truncating at each `,` (rightmost first), rebalancing each prefix. Only
/// yields candidates that close to a syntactically complete prefix.
fn truncate_at_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let commas: Vec<usize> = s.match_indices(',').map(|(i, _)| i).collect();
    for &pos in commas.iter().rev() {
        let prefix = &s[..pos];
        let candidate = append_missing_closers(prefix);
        if serde_json::from_str::<Value>(&candidate).is_ok() {
            out.push(candidate);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_valid_json_unchanged() {
        assert_eq!(fix_args_string(r#"{"command":"ls"}"#), r#"{"command":"ls"}"#);
        assert_eq!(fix_args_string(r#"{"a":1,"b":[2,3]}"#), r#"{"a":1,"b":[2,3]}"#);
        assert_eq!(fix_args_string("{}"), "{}");
    }

    #[test]
    fn closes_truncated_object() {
        assert_eq!(fix_args_string(r#"{"command":"echo hi""#), r#"{"command":"echo hi"}"#);
    }

    #[test]
    fn closes_truncated_string_and_object() {
        // The exact failure from tmp/err.json: EOF while parsing a string.
        let bad = r#"{"command":"echo broken"#;
        let fixed = fix_args_string(bad);
        assert!(serde_json::from_str::<Value>(&fixed).is_ok(), "fixed must parse: {fixed}");
        assert_eq!(
            fixed,
            r#"{"command":"echo broken"}"#
        );
    }

    #[test]
    fn closes_nested_truncation() {
        let bad = r#"{"commands":["ls","pwd","#;
        let fixed = fix_args_string(bad);
        let v: Value = serde_json::from_str(&fixed).expect("must parse");
        assert!(v["commands"].is_array());
    }

    #[test]
    fn escapes_bare_newline_in_string() {
        let bad = "{\"command\":\"echo\nhello\"}";
        let fixed = fix_args_string(bad);
        let v: Value = serde_json::from_str(&fixed).expect("must parse");
        assert_eq!(v["command"], "echo\nhello");
    }

    #[test]
    fn truncates_at_last_complete_pair() {
        // Truncated mid-value: the second value cannot be recovered, but the
        // first key/value pair can be salvaged.
        let bad = r#"{"a":"ok","b":"trun"#;
        let fixed = fix_args_string(bad);
        let v: Value = serde_json::from_str(&fixed).expect("must parse");
        assert_eq!(v["a"], "ok");
    }

    #[test]
    fn unrepairable_input_returned_unchanged() {
        // Garbage with no JSON structure: nothing to salvage, return as-is so
        // the downstream parser surfaces an honest error.
        let bad = "not json at all }}}";
        assert_eq!(fix_args_string(bad), bad);
    }

    #[test]
    fn repair_tool_call_modifies_args_in_place() {
        let mut tc = serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "bash", "arguments": "{\"command\":\"ls\""}
        });
        assert!(repair_tool_call(&mut tc));
        let args = tc["function"]["arguments"].as_str().unwrap();
        let v: Value = serde_json::from_str(args).unwrap();
        assert_eq!(v["command"], "ls");
    }

    #[test]
    fn repair_tool_calls_counts_only_modified() {
        let mut batch = vec![
            serde_json::json!({
                "id": "ok", "type": "function",
                "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
            }),
            serde_json::json!({
                "id": "bad", "type": "function",
                "function": {"name": "bash", "arguments": "{\"command\":\"echo"}
            }),
        ];
        let repaired = repair_tool_calls(&mut batch);
        assert_eq!(repaired, 1);
        // Untouched entry stays byte-identical.
        assert_eq!(
            batch[0]["function"]["arguments"],
            serde_json::json!("{\"command\":\"ls\"}")
        );
        // Fixed entry parses.
        let args = batch[1]["function"]["arguments"].as_str().unwrap();
        let v: Value = serde_json::from_str(args).unwrap();
        assert_eq!(v["command"], "echo");
    }

    #[test]
    fn dangling_backslash_dropped_before_close() {
        // Truncated mid-escape: trailing `\` must be dropped so the string can
        // be closed.
        let bad = r#"{"command":"echo \"#;
        let fixed = fix_args_string(bad);
        let v: Value = serde_json::from_str(&fixed).expect("must parse");
        assert_eq!(v["command"], r#"echo "#);
    }
}
