// TDD tests for gladiator-tui
//
// RED phase: these tests define the expected behavior before implementation.

use gladiator_core::message::Message;
use gladiator_tui::theme::Theme;
use gladiator_tui::state::{
    AppMessage, AppMessageRole, ChatState, InputState, ScrollState,
};
use gladiator_tui::event::bus_to_app_message;

// --- Theme tests ---

#[test]
fn theme_default_dark_colors() {
    let theme = Theme::default_dark();
    assert!(theme.primary.len() > 0);
    // opencode dark theme: primary = #fab283 (warm orange)
    assert_eq!(theme.primary, "#fab283");
    assert_eq!(theme.background, "#0a0a0a");
    assert_eq!(theme.background_panel, "#141414");
    assert_eq!(theme.text, "#eeeeee");
    assert_eq!(theme.text_muted, "#808080");
    assert_eq!(theme.success, "#7fd88f");
    assert_eq!(theme.error, "#e06c75");
    assert_eq!(theme.warning, "#f5a742");
    assert_eq!(theme.info, "#56b6c2");
    assert_eq!(theme.accent, "#9d7cd8");
    assert_eq!(theme.secondary, "#5c9cf5");
}

#[test]
fn theme_has_border_colors() {
    let theme = Theme::default_dark();
    assert_eq!(theme.border, "#484848");
    assert_eq!(theme.border_active, "#606060");
}

// --- InputState tests ---

#[test]
fn input_state_new_empty() {
    let input = InputState::new();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn input_state_type_chars() {
    let mut input = InputState::new();
    input.insert_char('h');
    input.insert_char('i');
    assert_eq!(input.buffer(), "hi");
    assert_eq!(input.cursor(), 2);
}

#[test]
fn input_state_backspace() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.backspace();
    assert_eq!(input.buffer(), "hell");
    assert_eq!(input.cursor(), 4);
}

#[test]
fn input_state_backspace_empty() {
    let mut input = InputState::new();
    input.backspace();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn input_state_cursor_left_right() {
    let mut input = InputState::new();
    input.insert_str("abc");
    assert_eq!(input.cursor(), 3);
    input.cursor_left();
    assert_eq!(input.cursor(), 2);
    input.cursor_left();
    assert_eq!(input.cursor(), 1);
    input.cursor_right();
    assert_eq!(input.cursor(), 2);
    // cannot go past end
    input.cursor_right();
    input.cursor_right();
    assert_eq!(input.cursor(), 3);
}

#[test]
fn input_state_insert_at_cursor() {
    let mut input = InputState::new();
    input.insert_str("ac");
    input.cursor_left();
    // cursor at 1, insert 'b' -> "abc"
    input.insert_char('b');
    assert_eq!(input.buffer(), "abc");
    assert_eq!(input.cursor(), 2);
}

#[test]
fn input_state_clear() {
    let mut input = InputState::new();
    input.insert_str("hello");
    input.clear();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn input_state_submit() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    let text = input.submit();
    assert_eq!(text, "hello world");
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

// --- ScrollState tests ---

#[test]
fn scroll_state_new() {
    let scroll = ScrollState::new();
    assert_eq!(scroll.offset(), 0);
    assert!(scroll.stick_to_bottom());
}

#[test]
fn scroll_state_scroll_down() {
    let mut scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    // Start stick_to_bottom = true, offset should be at max
    scroll.update_if_sticking();
    assert_eq!(scroll.offset(), 80);
    // Scroll up from bottom
    scroll.scroll_up();
    assert_eq!(scroll.offset(), 79);
    // Scroll down
    scroll.scroll_down();
    assert_eq!(scroll.offset(), 80);
}

#[test]
fn scroll_state_scroll_up() {
    let mut scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    scroll.update_if_sticking();
    // Should be at bottom (80)
    assert_eq!(scroll.offset(), 80);
    scroll.scroll_up();
    assert_eq!(scroll.offset(), 79);
    scroll.scroll_up();
    assert_eq!(scroll.offset(), 78);
    // Scroll to top
    scroll.scroll_to_top();
    assert_eq!(scroll.offset(), 0);
    scroll.scroll_up();
    assert_eq!(scroll.offset(), 0);
}

#[test]
fn scroll_state_scroll_to_bottom() {
    let mut scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    scroll.scroll_to_top();
    assert_eq!(scroll.offset(), 0);
    scroll.scroll_to_bottom();
    assert_eq!(scroll.offset(), 80);
}

#[test]
fn scroll_state_stick_to_bottom() {
    let scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    // stick_to_bottom defaults to true
    scroll.update_if_sticking();
    assert_eq!(scroll.offset(), 80);
    // More lines arrive
    scroll.set_total_lines(120);
    scroll.update_if_sticking();
    assert_eq!(scroll.offset(), 100);
}

#[test]
fn scroll_state_scroll_up_clears_stick() {
    let mut scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    scroll.update_if_sticking();
    assert!(scroll.stick_to_bottom());
    scroll.scroll_up();
    assert!(!scroll.stick_to_bottom());
    // New lines arrive, should NOT auto-scroll
    scroll.set_total_lines(120);
    scroll.update_if_sticking();
    assert_eq!(scroll.offset(), 79); // stayed at 79, didn't jump to 100
}

#[test]
fn scroll_state_page_up_down() {
    let mut scroll = ScrollState::new();
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    scroll.update_if_sticking();
    assert_eq!(scroll.offset(), 80);
    scroll.scroll_page_up();
    assert_eq!(scroll.offset(), 60);
    scroll.scroll_page_down();
    assert_eq!(scroll.offset(), 80);
    assert!(scroll.stick_to_bottom());
}

// --- ChatState / AppMessage tests ---

#[test]
fn chat_state_new_empty() {
    let chat = ChatState::new();
    assert_eq!(chat.message_count(), 0);
}

#[test]
fn chat_state_add_user_message() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::user("hello"));
    assert_eq!(chat.message_count(), 1);
    let msg = chat.message(0).unwrap();
    assert_eq!(msg.role, AppMessageRole::User);
    assert_eq!(msg.content, "hello");
}

#[test]
fn chat_state_add_assistant_message() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::assistant("I can help with that"));
    let msg = chat.message(0).unwrap();
    assert_eq!(msg.role, AppMessageRole::Assistant);
    assert_eq!(msg.content, "I can help with that");
}

#[test]
fn chat_state_add_tool_call_message() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::tool_call("bash", "ls -la", "file1.txt\nfile2.txt"));
    let msg = chat.message(0).unwrap();
    assert_eq!(msg.role, AppMessageRole::Tool);
    assert!(msg.content.contains("bash"));
    assert!(msg.content.contains("ls -la"));
}

#[test]
fn chat_state_add_error_message() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::error("Something went wrong"));
    let msg = chat.message(0).unwrap();
    assert_eq!(msg.role, AppMessageRole::Error);
    assert_eq!(msg.content, "Something went wrong");
}

#[test]
fn chat_state_append_to_last_message() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::assistant("Hello"));
    chat.append_to_last(" world");
    let msg = chat.message(0).unwrap();
    assert_eq!(msg.content, "Hello world");
}

#[test]
fn chat_state_append_creates_if_empty() {
    let mut chat = ChatState::new();
    chat.append_to_last("first");
    assert_eq!(chat.message_count(), 1);
    assert_eq!(chat.message(0).unwrap().content, "first");
}

#[test]
fn chat_state_clear() {
    let mut chat = ChatState::new();
    chat.add_message(AppMessage::user("a"));
    chat.add_message(AppMessage::user("b"));
    chat.clear();
    assert_eq!(chat.message_count(), 0);
}

// --- Event conversion tests ---

#[test]
fn event_user_input_to_app_message() {
    let msg = Message::text("user-input", "user", "hello there")
        .with_type("UserInput");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::User);
    assert_eq!(app_msg.content, "hello there");
}

#[test]
fn event_llm_stream_to_assistant() {
    let msg = Message::text("llm-stream", "llm-0", "generating...")
        .with_type("LlmStream");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::Assistant);
    assert_eq!(app_msg.content, "generating...");
}

#[test]
fn event_llm_stream_end_filtered() {
    // LlmStreamEnd is filtered out as noise
    let msg = Message::text("llm-stream", "llm-0", "done")
        .with_type("LlmStreamEnd");
    assert!(bus_to_app_message(&msg).is_none());
}

#[test]
fn event_llm_tool_call_to_tool() {
    // LlmToolCall (streaming delta) shows tool-building progress.
    // Payload is JSON with index, id, function.name, function.arguments.
    let msg = Message::new("llm-tool", "llm-0", serde_json::json!({
        "index": 0,
        "id": "call_1",
        "function": {
            "name": "bash",
            "arguments": "{\"command\": \"ls -la\"}"
        }
    }))
    .with_type("LlmToolCall");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::Tool);
    assert!(app_msg.content.contains("bash"));
    assert!(app_msg.content.contains("ls -la"));
}

#[test]
fn event_llm_tool_calls_filtered() {
    // LlmToolCalls (plural, final JSON) is filtered out.
    let msg = Message::text("llm-tool", "llm-0", "[{...}]")
        .with_type("LlmToolCalls");
    assert!(bus_to_app_message(&msg).is_none());
}

#[test]
fn event_stream_stats_filtered() {
    let msg = Message::text("llm-stats", "llm-0", "chars: 100")
        .with_type("StreamStats");
    assert!(bus_to_app_message(&msg).is_none());
}

#[test]
fn event_llm_tool_result_to_tool() {
    let msg = Message::text("llm-tool", "llm-0", "result: file1.txt")
        .with_type("LlmToolResult");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::Tool);
}

#[test]
fn event_error_to_error() {
    let msg = Message::text("error", "llm-0", "failed to connect")
        .with_type("Error");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::Error);
    assert_eq!(app_msg.content, "failed to connect");
}

#[test]
fn event_info_to_info() {
    let msg = Message::text("info", "agent", "starting iteration 1")
        .with_type("Info");
    let app_msg = bus_to_app_message(&msg).unwrap();
    assert_eq!(app_msg.role, AppMessageRole::Info);
}

#[test]
fn event_unknown_type_returns_none() {
    let msg = Message::text("unknown", "agent", "something");
    let app_msg = bus_to_app_message(&msg);
    assert!(app_msg.is_none());
}
