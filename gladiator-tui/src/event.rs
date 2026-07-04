use crate::state::{AppMessage, AppMessageRole};
use gladiator_core::message::Message;

/// Convert a bus Message to an AppMessage for display.
/// Returns None if the message type is not recognized or is noise.
pub fn bus_to_app_message(msg: &Message) -> Option<AppMessage> {
    let msg_type = msg.meta_type();
    let content = msg.payload_str().unwrap_or_default();

    match msg_type {
        Some(t) => match t {
            "UserInput" => Some(AppMessage::user(content)),
            "LlmStream" | "LlmThinking" | "LlmDump" => {
                Some(AppMessage::assistant(content))
            }
            // Filter out noise types
            "LlmStreamEnd" | "LlmToolCalls" | "StreamStats" => None,
            "LlmToolCall" => {
                // Tool call building progress — parse JSON payload
                let name = msg.payload.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = msg.payload.get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("");

                // If this is an edit_file / plan_edits call with complete
                // arguments, render the change as a unified-diff instead of
                // raw JSON.
                let parsed_args = serde_json::from_str::<serde_json::Value>(args).ok();
                let content = if !name.is_empty() && !args.is_empty() {
                    if let Some(ref p) = parsed_args {
                        if let Some(diff) = crate::diff_render::render_tool_diff(name, p) {
                            format!("{} \n{}", name, diff)
                        } else {
                            format!("{}({})", name, args)
                        }
                    } else {
                        // Arguments still being built (partial JSON) — show
                        // the in-progress call without a diff.
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
                Some(AppMessage {
                    role: AppMessageRole::Tool,
                    content,
                })
            }
            "LlmToolResult" => {
                Some(AppMessage {
                    role: AppMessageRole::Tool,
                    content,
                })
            }
            "Error" => Some(AppMessage::error(content)),
            "Warning" => Some(AppMessage {
                role: AppMessageRole::Error,
                content,
            }),
            "Info" => Some(AppMessage::info(content)),
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
    }
}
