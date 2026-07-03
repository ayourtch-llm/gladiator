use crate::state::{AppMessage, AppMessageRole};
use gladiator_core::message::Message;

/// Convert a bus Message to an AppMessage for display.
/// Returns None if the message type is not recognized/displayable.
pub fn bus_to_app_message(msg: &Message) -> Option<AppMessage> {
    let msg_type = msg.meta_type();
    let content = msg.payload_str().unwrap_or_default();

    match msg_type {
        Some(t) => match t {
            "UserInput" => Some(AppMessage::user(content)),
            "LlmStream" | "LlmStreamEnd" | "LlmThinking" | "LlmDump" => {
                Some(AppMessage::assistant(content))
            }
            "LlmToolCall" | "LlmToolResult" | "LlmToolCalls" => {
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
            "Log" => Some(AppMessage::system(content)),
            "Interrupt" => Some(AppMessage::system(format!(
                "[interrupt] {}",
                content
            ))),
            "Continue" => Some(AppMessage::system(format!(
                "[continue] {}",
                content
            ))),
            "StreamStats" => Some(AppMessage::info(content)),
            _ => None,
        },
        None => None,
    }
}
