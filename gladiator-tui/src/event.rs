use crate::state::{AppMessage, AppMessageRole};
use gladiator_core::message::Message;

/// Convert a bus Message to an AppMessage for display.
/// Returns None if the message type is not recognized or is noise.
pub fn bus_to_app_message(msg: &Message) -> Option<AppMessage> {
    let msg_type = msg.meta_type();
    let content = msg.payload_str().unwrap_or_default();
    // Read subagent indentation depth from metadata (0 when at top level).
    let depth = msg.meta_depth();

    let result: Option<AppMessage> = match msg_type {
        Some(t) => match t {
            "UserInput" => Some(AppMessage::user(content)),
            "LlmStream" | "LlmThinking" | "LlmDump" => {
                Some(AppMessage::assistant(content))
            }
            // Filter out noise types
            "LlmStreamEnd" | "LlmToolCalls" | "StreamStats" | "LlmRequestSent" => None,
            "LlmToolCall" => {
                let name = msg.payload.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = msg.payload.get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("");

                // Stable id for matching: prefer the LLM-provided call id,
                // fall back to index-based synthetic key.
                let tool_id = msg.payload.get("id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        msg.payload.get("index")
                            .and_then(|i| i.as_u64())
                            .map(|idx| format!("__idx_{}", idx))
                    });

                // If this is an edit_file / plan_edits call with complete
                // arguments, render the change as a unified-diff instead of
                // raw JSON.
                let parsed_args = serde_json::from_str::<serde_json::Value>(args).ok();
                let content = if !name.is_empty() && !args.is_empty() {
                    if let Some(ref p) = parsed_args {
                        if let Some(diff) = crate::diff_render::render_tool_diff(name, p) {
                            format!("{}\n{}", name, diff)
                        } else if let Some(cmd_line) = crate::diff_render::render_tool_call(name, p) {
                            cmd_line
                        } else {
                            format!("{}({})", name, args)
                        }
                    } else {
                        // Arguments still being built (partial JSON).
                        if !name.is_empty() && parsed_args.is_none() && !args.contains("{") {
                            format!("{}(building...)", name)
                        } else {
                            format!("{}({})", name, args)
                        }
                    }
                } else if !name.is_empty() {
                    format!("{}(building...)", name)
                } else {
                    "building...".to_string()
                };
                Some(AppMessage::tool_with_meta(content, tool_id, if !name.is_empty() { Some(name) } else { None }))
            }
            "LlmToolResult" => {
                // LlmToolResult payload is a string of the form:
                //   "  [tool_result] func_name(tool_call_id) => result_text"
                // Parse out tool_call_id for matching.
                let tool_id = parse_tool_result_id(&content);
                Some(AppMessage::tool(content, tool_id))
            }
            "Error" => Some(AppMessage::error(content)),
            "Warning" => Some(AppMessage {
                role: AppMessageRole::Error,
                content,
                tool_id: None,
                tool_name: None,
                tool_kind: None,
                depth,
            }),
            "Info" => {
                // Support both legacy plain-text and structured JSON payloads.
                if let Some(text) = content.strip_prefix("Calling tool:") {
                    return Some(AppMessage::info(format!("Calling tool: {}", text.trim())).with_depth(depth));
                }
                // Structured form: {"id": ..., "name": ..., "text": "..."}
                if msg.payload.is_object() {
                    let id = msg.payload.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = msg.payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let text = msg.payload.get("text").and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("Calling tool: {}", name));
                    // If this is a "Calling tool:" dispatch, tag with id for
                    // coalescing into the matching [tool] placeholder.
                    if text.starts_with("Calling tool:") {
                        let display = text.trim_start_matches("Calling tool: ").to_string();
                        Some(AppMessage::tool(display, if !id.is_empty() { Some(id) } else { None }))
                    } else {
                        Some(AppMessage::info(text))
                    }
                } else {
                    Some(AppMessage::info(content))
                }
            }
            "PersistenceResponse" => {
                let success = msg.payload.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                let message = msg.payload.get("message").and_then(|v| v.as_str()).unwrap_or("Unknown response").to_string();
                if success {
                    Some(AppMessage::system(message))
                } else {
                    Some(AppMessage::error(message))
                }
            }
            "Log" => Some(AppMessage::system(content)),
            "Interrupt" => Some(AppMessage::system(format!(
                "[interrupt] {}",
                content
            ))),
            "Continue" => Some(AppMessage::system(format!(
                "[continue] {}",
                content
            ))),
            _ => None,
        },
        None => None,
    };
    result.map(|m| {
        if m.depth == 0 && depth > 0 {
            m.with_depth(depth)
        } else {
            m
        }
    })
}

/// Parse the tool_call_id from an LlmToolResult display string of the form:
///   "  [tool_result] func_name(tool_call_id) => result_text"
/// Returns the id if found.
fn parse_tool_result_id(content: &str) -> Option<String> {
    // Find "(...)" — the content inside parens is the tool_call_id
    let start = content.find('(')?;
    let end = content[start..].find(')').map(|p| p + start)?;
    if end > start + 1 {
        Some(content[start + 1..end].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_result_id_basic() {
        let s = "  [tool_result] bash(call_abc) => done";
        assert_eq!(parse_tool_result_id(s), Some("call_abc".to_string()));
    }

    #[test]
    fn parse_result_id_error_form() {
        let s = "  [tool_error] edit_file(__idx_0) => failed";
        assert_eq!(parse_tool_result_id(s), Some("__idx_0".to_string()));
    }
}
