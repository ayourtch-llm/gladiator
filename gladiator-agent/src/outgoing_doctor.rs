//! Outgoing request doctoring — sanitize tool_calls arguments before sending
//! to the LLM.
//!
//! When conversation history contains a corrupted `tool_calls[].function.arguments`
//! string (e.g. from a dropped SSE chunk), replaying that message causes the
//! LLM server to reject the **entire** request with HTTP 500, bricking the agent.
//!
//! Strategy: for each assistant message containing tool_calls:
//! - If ALL args parse as JSON → canonical re-serialize (no-op semantically).
//! - If ANY arg fails to parse → delete that entire assistant message AND every
//!   matching role=tool result (by tool_call_id), inserting one synthetic
//!    assistant + user pair in their place. This preserves alternation, gives the
//!    model context about what failed, and avoids fragile char-by-char JSON repair.

use serde_json::Value;

#[derive(Debug)]
pub struct RepairRecord {
    pub msg_index: usize,
    pub tool_names: Vec<String>,
}

/// Doctor all assistant tool_calls in the message array. Mutates `messages`
/// in place — may grow or shrink the vector as broken pairs are replaced.
pub fn doctor_messages(messages: &mut Vec<Value>) -> Vec<RepairRecord> {
    use std::collections::HashSet;

    let mut repairs = Vec::new();

    // Pass 1: identify which assistant messages have ≥1 unparseable args,
    // and collect ALL tc_ids from those broken assistants so we can skip
    // their matching tool-result messages too.
    let mut orphan_tc_ids: HashSet<String> = HashSet::new();
    let mut broken_indices: HashSet<usize> = HashSet::new();

    for (i, msg) in messages.iter().enumerate() {
        if !is_assistant_with_tool_calls(msg) {
            continue;
        }
        let tcs = get_tool_calls(msg).unwrap_or_default();
        let any_broken = tcs
            .iter()
            .any(|tc| tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())
                .is_some_and(|_| {
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str());
                    match args {
                        Some(s) => serde_json::from_str::<Value>(s).is_err(),
                        None => true,
                    }
                }));

        if any_broken {
            broken_indices.insert(i);
            for tc in &tcs {
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    orphan_tc_ids.insert(id.to_string());
                }
            }
        }
    }

    // Pass 2: rebuild the message array.
    let mut new_msgs = Vec::with_capacity(messages.len());

    for (i, msg) in messages.iter().enumerate() {
        if broken_indices.contains(&i) {
            // Collect tool names from this assistant's tool_calls.
            let tcs = get_tool_calls(msg).unwrap_or_default();
            let tool_names: Vec<String> = tcs
                .iter()
                .filter_map(|tc| {
                    tc.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();

            repairs.push(RepairRecord { msg_index: i, tool_names: tool_names.clone() });

            // Insert synthetic pair preserving alternation.
            new_msgs.push(serde_json::json!({
                "role": "assistant",
                "content": format!(
                    "[tool call to {} failed: arguments were corrupted during streaming and could not be parsed]",
                    if tool_names.len() == 1 {
                        tool_names[0].clone()
                    } else {
                        format!("{} (and possibly others)", tool_names.join(", "))
                    }
                ),
            }));
            new_msgs.push(serde_json::json!({
                "role": "user",
                "content": "[the tool call arguments were malformed JSON — likely a streaming chunk was dropped. Please retry the tool call if needed.]"
            }));

            continue;
        }

        // Skip orphaned tool-result messages whose assistant was deleted.
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" {
            if let Some(tc_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                if orphan_tc_ids.contains(tc_id) {
                    continue;
                }
            }
        }

        // For valid assistant messages: canonical re-serialize args.
        let mut cloned = msg.clone();
        if is_assistant_with_tool_calls(&cloned) {
            if let Some(tcs_val) = cloned.get("tool_calls").cloned() {
                let mut tcs_vec: Vec<Value> =
                    serde_json::from_value(tcs_val).unwrap_or_default();
                for tc in &mut tcs_vec {
                    if let Some(args_str) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .map(String::from)
                    {
                        if let Ok(v) = serde_json::from_str::<Value>(&args_str) {
                            tc["function"]["arguments"] =
                                Value::String(serde_json::to_string(&v).unwrap());
                        }
                    }
                }
                if let Some(obj) = cloned.as_object_mut() {
                    obj.insert(
                        "tool_calls".to_string(),
                        serde_json::to_value(&tcs_vec).unwrap_or(Value::Null),
                    );
                }
            }
        }

        new_msgs.push(cloned);
    }

    *messages = new_msgs;
    repairs
}

fn is_assistant_with_tool_calls(msg: &Value) -> bool {
    msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
        && msg.get("tool_calls").is_some()
}

fn get_tool_calls(msg: &Value) -> Option<Vec<Value>> {
    let tcs = msg.get("tool_calls")?;
    serde_json::from_value(tcs.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_valid_args_no_repair() {
        let mut msgs = vec![serde_json::json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
            }]
        })];
        let repairs = doctor_messages(&mut msgs);
        assert!(repairs.is_empty());
        // Args should be canonically re-serialized (unchanged for already-canonical input).
        let args = &msgs[0]["tool_calls"][0]["function"]["arguments"];
        assert_eq!(args.as_str().unwrap(), "{\"command\":\"ls\"}");
    }

    #[test]
    fn broken_args_replaced_with_synthetic_pair() {
        // The exact case from tmp/broken.json:
        // `{"file_path":"mcp-rlsp/src/analyzer/client.rs"15,"offset":340}`
        let mut msgs = vec![
            serde_json::json!({"role": "user", "content": "read the file"}),
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "read_file",
                        "arguments": "{\"file_path\":\"mcp-rlsp/src/analyzer/client.rs\"15,\"offset\":340}"}
                }]
            }),
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "call_1",
                "name": "read_file",
                "content": "Error parsing arguments"
            }),
        ];
        let repairs = doctor_messages(&mut msgs);
        assert_eq!(repairs.len(), 1, "one repair expected");
        // The broken assistant + tool result should be replaced by synthetic pair.
        assert_eq!(msgs.len(), 3); // user + synthetic_assistant + synthetic_user
        assert_eq!(msgs[0]["role"], "user");       // original user preserved
        assert_eq!(msgs[1]["role"], "assistant");   // synthetic assistant
        assert_eq!(msgs[2]["role"], "user");         // synthetic user
    }

    #[test]
    fn multiple_broken_tool_calls_replaced() {
        let mut msgs = vec![
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [
                    {"id":"a","type":"function","function":{"name":"bash","arguments":"{\"cmd\":\"ls\""}},
                    {"id":"b","type":"function","function":{"name":"read","arguments":"GARBAGE"}}
                ]
            }),
            serde_json::json!({"role":"tool","tool_call_id":"a","content":"ok"}),
            serde_json::json!({"role":"tool","tool_call_id":"b","content":"err"}),
        ];
        let repairs = doctor_messages(&mut msgs);
        assert_eq!(repairs.len(), 1, "one repair (whole message replaced)");
        // Both tool results should be gone.
        assert_eq!(msgs.len(), 2); // synthetic assistant + user only
    }

    #[test]
    fn mixed_valid_and_broken_replaces_whole_message() {
        let mut msgs = vec![
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [
                    {"id":"ok","type":"function","function":{"name":"bash","arguments":"{\"cmd\":\"ls\"}"}},
                    {"id":"bad","type":"function","function":{"name":"read","arguments":"GARBAGE"}}
                ]
            }),
            serde_json::json!({"role":"tool","tool_call_id":"ok","content":"file list"}),
            serde_json::json!({"role":"tool","tool_call_id":"bad","content":"error"}),
        ];
        let _ = doctor_messages(&mut msgs);
        // Whole message replaced — valid call is also lost (acceptable: sendability > fidelity).
        assert_eq!(msgs.len(), 2); // synthetic pair only
    }

    #[test]
    fn non_assistant_and_non_tool_untouched() {
        let mut msgs = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "assistant", "content": "hi there"}),  // no tool_calls
            serde_json::json!({
                "role": "tool",
                "tool_call_id": "x",
                "name": "bash",
                "content": "output"
            }),
        ];
        let repairs = doctor_messages(&mut msgs);
        assert!(repairs.is_empty());
    }

    #[test]
    fn round_trip_serialization() {
        let mut msgs = vec![
            serde_json::json!({"role":"user","content":"go"}),
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "read_file",
                        "arguments": "{\"file_path\":\"x.rs\"15}"}
                }]
            }),
        ];
        let _ = doctor_messages(&mut msgs);
        let s = serde_json::to_string(&msgs).unwrap();
        let reparsed: Vec<Value> = serde_json::from_str(&s).unwrap();
        assert!(!reparsed.is_empty());
    }
}
