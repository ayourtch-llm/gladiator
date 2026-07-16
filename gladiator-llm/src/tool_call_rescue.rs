//! Rescue tool calls that a model emitted as literal text instead of
//! structured `tool_calls` deltas.
//!
//! Some local models (Qwen3-Coder style templates and their finetunes)
//! occasionally emit their native tool-call markup inside the reasoning
//! channel, before closing the think block:
//!
//! ```text
//! <tool_call>
//! <function=read_file>
//! <parameter=file_path>
//! /path/to/file.rs
//! </parameter>
//! </function>
//! </tool_call>
//! ```
//!
//! The serving side (e.g. llama.cpp) only extracts structured tool calls from
//! the content section, so these pass through as plain text and the turn
//! stalls. When a stream finishes with NO structured tool calls, we scan the
//! accumulated text and reasoning for well-formed blocks like the above and
//! convert them into OpenAI-format tool calls.
//!
//! To keep false positives out (e.g. the model merely *discussing* the
//! syntax), a block is only rescued when it is fully well-formed AND names a
//! tool that was actually offered in the request.

use std::collections::HashSet;

/// Extract tool calls from raw text. `known_tools` is the set of tool names
/// offered in the request; blocks naming anything else are ignored.
/// Returns OpenAI-format tool call values:
/// `{"id", "type": "function", "function": {"name", "arguments": <JSON string>}}`.
pub fn extract_tool_calls(
    text: &str,
    known_tools: &HashSet<String>,
) -> Vec<serde_json::Value> {
    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut rest = text;

    while let Some(start) = rest.find("<tool_call>") {
        let after_open = &rest[start + "<tool_call>".len()..];
        let Some(end) = after_open.find("</tool_call>") else {
            break; // truncated block (stream cut off mid-call) — not rescuable
        };
        let block = &after_open[..end];
        rest = &after_open[end + "</tool_call>".len()..];

        let Some((name, args)) = parse_block(block) else {
            continue;
        };
        if !known_tools.contains(&name) {
            continue;
        }
        let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
        // The model sometimes loops, re-emitting the identical call; collapse dupes.
        if !seen.insert((name.clone(), args_str.clone())) {
            continue;
        }
        calls.push(serde_json::json!({
            "id": format!("rescued-{}", calls.len()),
            "type": "function",
            "function": {
                "name": name,
                "arguments": args_str,
            }
        }));
    }
    calls
}

/// Parse one `<function=NAME> <parameter=KEY> VALUE </parameter> ... </function>`
/// block body. Returns None unless the block is fully well-formed.
fn parse_block(block: &str) -> Option<(String, serde_json::Value)> {
    let fn_start = block.find("<function=")?;
    let after_fn = &block[fn_start + "<function=".len()..];
    let fn_name_end = after_fn.find('>')?;
    let name = after_fn[..fn_name_end].trim().to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let mut body = &after_fn[fn_name_end + 1..];
    // The closing </function> must be present for the block to count as complete.
    let fn_close = body.rfind("</function>")?;
    body = &body[..fn_close];

    let mut args = serde_json::Map::new();
    let mut rest = body;
    while let Some(p_start) = rest.find("<parameter=") {
        let after_p = &rest[p_start + "<parameter=".len()..];
        let key_end = after_p.find('>')?;
        let key = after_p[..key_end].trim().to_string();
        let after_key = &after_p[key_end + 1..];
        let p_close = after_key.find("</parameter>")?;
        let raw_value = trim_one_newline(&after_key[..p_close]);
        args.insert(key, coerce_value(raw_value));
        rest = &after_key[p_close + "</parameter>".len()..];
    }
    Some((name, serde_json::Value::Object(args)))
}

/// Values are raw text between the tag lines; strip the framing newlines
/// only, preserving any interior whitespace the value legitimately contains.
fn trim_one_newline(s: &str) -> &str {
    let s = s.strip_prefix("\r\n").or_else(|| s.strip_prefix('\n')).unwrap_or(s);
    s.strip_suffix("\r\n").or_else(|| s.strip_suffix('\n')).unwrap_or(s)
}

/// Best-effort typing: the markup carries no types, so parameters that read
/// as JSON numbers/booleans/null/objects/arrays are passed through typed;
/// everything else stays a string.
fn coerce_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) if !v.is_string() => v,
        _ => serde_json::Value::String(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rescues_single_call() {
        let text = "<tool_call>\n<function=read_file>\n<parameter=file_path>\n/tmp/x.rs\n</parameter>\n</function>\n</tool_call>";
        let calls = extract_tool_calls(text, &tools(&["read_file"]));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "read_file");
        let args: serde_json::Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["file_path"], "/tmp/x.rs");
    }

    #[test]
    fn rescues_multiple_calls_and_dedupes() {
        let one = "<tool_call>\n<function=read_file>\n<parameter=file_path>\n/a.rs\n</parameter>\n</function>\n</tool_call>";
        let two = "<tool_call>\n<function=read_file>\n<parameter=file_path>\n/b.rs\n</parameter>\n</function>\n</tool_call>";
        let text = format!("thinking...{one}\n{two}\n{one}");
        let calls = extract_tool_calls(&text, &tools(&["read_file"]));
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn ignores_unknown_tools_and_truncated_blocks() {
        let unknown = "<tool_call>\n<function=rm_rf>\n<parameter=path>\n/\n</parameter>\n</function>\n</tool_call>";
        let truncated = "<tool_call>\n<function=read_file>\n<parameter=file_path>\n/a.rs";
        assert!(extract_tool_calls(unknown, &tools(&["read_file"])).is_empty());
        assert!(extract_tool_calls(truncated, &tools(&["read_file"])).is_empty());
    }

    #[test]
    fn types_numeric_and_bool_params() {
        let text = "<tool_call>\n<function=grep>\n<parameter=pattern>\nfoo bar\n</parameter>\n<parameter=max_results>\n10\n</parameter>\n<parameter=case_sensitive>\ntrue\n</parameter>\n</function>\n</tool_call>";
        let calls = extract_tool_calls(text, &tools(&["grep"]));
        let args: serde_json::Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["pattern"], "foo bar");
        assert_eq!(args["max_results"], 10);
        assert_eq!(args["case_sensitive"], true);
    }

    #[test]
    fn multiline_parameter_value_preserved() {
        let text = "<tool_call>\n<function=write_file>\n<parameter=content>\nline1\nline2\n</parameter>\n</function>\n</tool_call>";
        let calls = extract_tool_calls(text, &tools(&["write_file"]));
        let args: serde_json::Value =
            serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["content"], "line1\nline2");
    }
}
