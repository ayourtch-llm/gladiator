// TDD tests for gladiator-tui
//
// RED phase: these tests define the expected behavior before implementation.

use gladiator_core::message::Message;
use gladiator_tui::theme::Theme;
use gladiator_tui::state::{
    AppMessage, AppMessageRole, ChatState, InputState, ScrollState,
};
use gladiator_tui::event::bus_to_app_message;
use gladiator_tui::app::App;

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

#[test]
fn input_state_insert_newline() {
    let mut input = InputState::new();
    input.insert_str("line1");
    input.insert_newline();
    input.insert_str("line2");
    assert_eq!(input.buffer(), "line1\nline2");
    assert_eq!(input.cursor(), 11); // 5 + 1 + 5 = 11 bytes
}

#[test]
fn input_state_multi_line_cursor_left_right() {
    let mut input = InputState::new();
    input.insert_str("ab\ncd");
    // cursor at end (5)
    assert_eq!(input.cursor(), 5);
    // left past newline
    input.cursor_left();
    assert_eq!(input.cursor(), 4); // before 'd'
    input.cursor_left();
    assert_eq!(input.cursor(), 3); // before 'c'
    input.cursor_left();
    assert_eq!(input.cursor(), 2); // before '\n'
    input.cursor_left();
    assert_eq!(input.cursor(), 1); // before 'b'
    input.cursor_left();
    assert_eq!(input.cursor(), 0);
    input.cursor_left(); // can't go below 0
    assert_eq!(input.cursor(), 0);
    // right past newline
    input.cursor_right();
    assert_eq!(input.cursor(), 1);
    input.cursor_right();
    assert_eq!(input.cursor(), 2);
    input.cursor_right();
    assert_eq!(input.cursor(), 3); // past '\n'
    input.cursor_right();
    assert_eq!(input.cursor(), 4);
    input.cursor_right();
    assert_eq!(input.cursor(), 5);
}

#[test]
fn input_state_multi_line_backspace_across_newline() {
    let mut input = InputState::new();
    input.insert_str("line1\nline2");
    // cursor at end
    input.cursor_left(); // before '2'
    input.cursor_left(); // before 'n'
    input.cursor_left(); // before 'i'
    input.cursor_left(); // before 'e' (second 'e' in line1)
    input.cursor_left(); // before '\n'
    input.backspace(); // remove '\n'
    assert_eq!(input.buffer(), "line1line2");
    assert_eq!(input.cursor(), 5);
}

// --- Command history tests ---

#[test]
fn input_state_history_basic() {
    let mut input = InputState::new();
    input.insert_str("first command");
    let _ = input.submit();
    input.insert_str("second command");
    let _ = input.submit();
    assert_eq!(input.history().len(), 2);
    assert_eq!(input.history()[0], "first command");
    assert_eq!(input.history()[1], "second command");
}

#[test]
fn input_state_history_prev_next() {
    let mut input = InputState::new();
    input.insert_str("cmd1");
    let _ = input.submit();
    input.insert_str("cmd2");
    let _ = input.submit();

    // Empty buffer, press Up -> go to last command
    input.history_prev();
    assert_eq!(input.buffer(), "cmd2");
    assert_eq!(input.cursor(), input.buffer().len());

    // Press Up again -> go to first command
    input.history_prev();
    assert_eq!(input.buffer(), "cmd1");

    // Press Down -> go to second command
    input.history_next();
    assert_eq!(input.buffer(), "cmd2");

    // Press Down again -> back to empty input
    input.history_next();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn input_state_history_prev_empty() {
    let mut input = InputState::new();
    // No history yet, pressing Up should do nothing
    input.history_prev();
    assert_eq!(input.buffer(), "");
    assert_eq!(input.cursor(), 0);
}

#[test]
fn input_state_history_prev_then_submit() {
    let mut input = InputState::new();
    input.insert_str("cmd1");
    let _ = input.submit();
    input.insert_str("cmd2");
    let _ = input.submit();

    // Navigate to previous command
    input.history_prev();
    assert_eq!(input.buffer(), "cmd2");

    // Submit it
    let text = input.submit();
    assert_eq!(text, "cmd2");
    assert_eq!(input.history().len(), 3);
    assert_eq!(input.history()[2], "cmd2");
    assert_eq!(input.buffer(), "");
}

#[test]
fn input_state_history_no_duplicates_consecutive() {
    let mut input = InputState::new();
    input.insert_str("cmd");
    let _ = input.submit();
    // Submit same command again
    input.insert_str("cmd");
    let _ = input.submit();
    // Should have 2 entries (we don't deduplicate)
    assert_eq!(input.history().len(), 2);
}

#[test]
fn input_state_history_reset_on_submit() {
    let mut input = InputState::new();
    input.insert_str("cmd1");
    let _ = input.submit();
    input.insert_str("cmd2");
    let _ = input.submit();

    // Navigate up
    input.history_prev();
    assert_eq!(input.buffer(), "cmd2");

    // Submit
    let _ = input.submit();
    assert_eq!(input.buffer(), "");

    // History index should be reset — pressing Up should go to latest
    input.history_prev();
    assert_eq!(input.buffer(), "cmd2"); // the one we just submitted
}

// --- Paste tests ---

#[test]
fn input_state_insert_str_single_line() {
    let mut input = InputState::new();
    input.insert_str("hello world");
    assert_eq!(input.buffer(), "hello world");
    assert_eq!(input.cursor(), 11);
}

#[test]
fn input_state_insert_str_multi_line_paste() {
    let mut input = InputState::new();
    // Simulate pasting "line1\nline2\nline3"
    input.insert_str("line1\nline2\nline3");
    assert_eq!(input.buffer(), "line1\nline2\nline3");
    assert_eq!(input.cursor(), 17); // 5+1+5+1+5 = 17
}

#[test]
fn input_state_insert_str_at_cursor_position() {
    let mut input = InputState::new();
    input.insert_str("ac");
    input.cursor_left(); // cursor at 1
    input.insert_str("bd"); // insert "bd" at cursor
    assert_eq!(input.buffer(), "abdc");
    assert_eq!(input.cursor(), 3);
}

#[test]
fn input_state_insert_str_crlf_normalized() {
    let mut input = InputState::new();
    // Simulate paste with \r\n line endings — insert_str inserts as-is
    // (normalization happens in the event handler, not InputState)
    input.insert_str("line1\r\nline2");
    // The \r\n is inserted literally; the app event handler normalizes before calling insert_str
    assert_eq!(input.buffer(), "line1\r\nline2");
}

#[test]
fn input_state_paste_then_continue_typing() {
    let mut input = InputState::new();
    input.insert_str("hello\n");
    input.insert_char('w');
    input.insert_char('o');
    input.insert_char('r');
    input.insert_char('l');
    input.insert_char('d');
    assert_eq!(input.buffer(), "hello\nworld");
    assert_eq!(input.cursor(), 11);
}

// --- set_buffer tests ---

#[test]
fn input_state_set_buffer_loads_text() {
    let mut input = InputState::new();
    input.set_buffer("retracted text");
    assert_eq!(input.buffer(), "retracted text");
    assert_eq!(input.cursor(), "retracted text".len());
}

#[test]
fn input_state_set_buffer_multiline() {
    let mut input = InputState::new();
    input.insert_char('x');
    input.set_buffer("line1\nline2");
    assert_eq!(input.buffer(), "line1\nline2");
    // Cursor at end
    assert_eq!(input.cursor(), 11);
}

#[test]
fn input_state_set_buffer_clears_history_index() {
    let mut input = InputState::new();
    input.insert_str("first");
    let _ = input.submit(); // pushes to history, clears buffer
    input.history_prev();   // loads "first" into buffer, sets history_index
    assert_eq!(input.buffer(), "first");
    input.set_buffer("replaced");
    assert_eq!(input.buffer(), "replaced");
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

#[test]
fn scroll_state_h_offset() {
    let mut scroll = ScrollState::new();
    assert_eq!(scroll.h_offset(), 0);
    scroll.scroll_right();
    assert_eq!(scroll.h_offset(), 1);
    scroll.scroll_right();
    assert_eq!(scroll.h_offset(), 2);
    scroll.scroll_left();
    assert_eq!(scroll.h_offset(), 1);
    scroll.scroll_left();
    assert_eq!(scroll.h_offset(), 0);
    // Can't go below 0
    scroll.scroll_left();
    assert_eq!(scroll.h_offset(), 0);
}

#[test]
fn scroll_state_h_offset_reset_on_stick() {
    let mut scroll = ScrollState::new();
    scroll.scroll_right();
    scroll.scroll_right();
    assert_eq!(scroll.h_offset(), 2);
    // stick_to_bottom should reset h_offset
    scroll.set_total_lines(100);
    scroll.set_visible_height(20);
    scroll.update_if_sticking();
    assert_eq!(scroll.h_offset(), 0);
}

#[test]
fn scroll_state_h_offset_reset_on_scroll_to_bottom() {
    let mut scroll = ScrollState::new();
    scroll.scroll_right();
    scroll.scroll_right();
    assert_eq!(scroll.h_offset(), 2);
    scroll.scroll_to_bottom();
    assert_eq!(scroll.h_offset(), 0);
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

// --- Retract pending messages tests ---

#[test]
fn retract_request_flag_set_on_up_with_pending_and_empty_input() {
    use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
    let mut app = App::new(Theme::default_dark());
    // Add a pending message
    app.add_pending_message("hello world".to_string());

    // Press Up — should set retract_requested and clear local pending list.
    assert!(!app.take_retract_request()); // initially false
    let _ = app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(app.pending_messages().is_empty(), "local pending cleared");
    assert!(app.take_retract_request(), "retract flag set after Up key");
}

#[test]
fn retract_not_triggered_with_nonempty_input() {
    use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
    let mut app = App::new(Theme::default_dark());
    // Add a pending message and some input text
    app.add_pending_message("hello".to_string());
    app.input_mut().insert_str("draft");

    let _ = app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(!app.take_retract_request(), "retract not triggered with non-empty buffer");
}

#[test]
fn retract_not_triggered_with_no_pending() {
    use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
    let mut app = App::new(Theme::default_dark());

    let _ = app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(!app.take_retract_request(), "retract not triggered with no pending");
}

#[test]
fn retract_flag_resets_after_consume() {
    use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};
    let mut app = App::new(Theme::default_dark());
    app.add_pending_message("msg".to_string());

    let _ = app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(app.take_retract_request(), "first consume should be true");
    // Second call resets to false
    assert!(!app.take_retract_request(), "second consume should be false");
}

#[test]
fn retrieved_pending_loads_into_input() {
    let mut app = App::new(Theme::default_dark());

    // Simulate agent sending back RetrievedPending with joined text as JSON
    let msg = Message::new("agent:stream", "gladiator-agent-0",
        serde_json::json!({"text": "line1\nline2"}))
        .with_type("RetrievedPending");
    app.handle_bus_message(&msg);

    assert_eq!(app.input().buffer(), "line1\nline2");
}
