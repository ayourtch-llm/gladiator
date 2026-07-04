use crate::commands::{parse_tui_command, TuiCommand};
use crate::event::bus_to_app_message;
use crate::state::{AppMessage, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use crate::render::Renderer;
use gladiator_core::bus::Bus;
use gladiator_core::config::TopicsConfig;
use gladiator_core::message::Message;
use std::io;
use std::time::{Duration, Instant};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event as CrosstermEvent, KeyCode, KeyEvent,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::debug;

pub struct App {
    chat: ChatState,
    input: InputState,
    scroll: ScrollState,
    renderer: Renderer,
    status: String,
    should_quit: bool,
    last_stream_type: Option<String>,
    last_tool_call_index: Option<usize>,
}

impl App {
    pub fn new(theme: Theme) -> Self {
        Self {
            chat: ChatState::new(),
            input: InputState::new(),
            scroll: ScrollState::new(),
            renderer: Renderer::new(theme),
            status: String::new(),
            should_quit: false,
            last_stream_type: None,
            last_tool_call_index: None,
        }
    }

    pub fn chat(&self) -> &ChatState {
        &self.chat
    }

    pub fn chat_mut(&mut self) -> &mut ChatState {
        &mut self.chat
    }

    pub fn input(&self) -> &InputState {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut InputState {
        &mut self.input
    }

    pub fn scroll(&self) -> &ScrollState {
        &self.scroll
    }

    pub fn scroll_mut(&mut self) -> &mut ScrollState {
        &mut self.scroll
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<String> {
        let ctrl = key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(crossterm::event::KeyModifiers::ALT);
        let shift = key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT);

        match key.code {
            // Quit: Ctrl+C, Ctrl+Q
            KeyCode::Char('c') if ctrl => {
                self.quit();
                None
            }
            KeyCode::Char('q') if ctrl => {
                self.quit();
                None
            }

            // Enter: plain = submit, Shift/Alt+Enter = newline
            KeyCode::Enter if shift || alt => {
                self.input.insert_newline();
                None
            }
            KeyCode::Enter => {
                let text = self.input.submit();
                if !text.is_empty() {
                    self.chat.add_message(AppMessage::user(&text));
                    self.scroll.scroll_to_bottom();
                    Some(text)
                } else {
                    None
                }
            }

            // Ctrl+J = newline
            KeyCode::Char('j') if ctrl => {
                self.input.insert_newline();
                None
            }

            // Emacs line-editing: Ctrl combos
            KeyCode::Char('a') if ctrl => {
                self.input.cursor_line_start();
                None
            }
            KeyCode::Char('e') if ctrl => {
                self.input.cursor_line_end();
                None
            }
            KeyCode::Char('b') if ctrl => {
                self.input.cursor_left();
                None
            }
            KeyCode::Char('f') if ctrl => {
                self.input.cursor_right();
                None
            }
            KeyCode::Char('d') if ctrl => {
                self.input.delete_char_forward();
                None
            }
            KeyCode::Char('h') if ctrl => {
                self.input.backspace();
                None
            }
            KeyCode::Char('k') if ctrl => {
                self.input.kill_to_end_of_line();
                None
            }
            KeyCode::Char('u') if ctrl => {
                self.input.kill_to_start_of_line();
                None
            }
            KeyCode::Char('w') if ctrl => {
                self.input.kill_word_backward();
                None
            }
            KeyCode::Char('y') if ctrl => {
                self.input.yank();
                None
            }
            KeyCode::Char('p') if ctrl => {
                self.input.history_prev();
                None
            }
            KeyCode::Char('n') if ctrl => {
                self.input.history_next();
                None
            }

            // Alt combos (word movement, word kill, history jump)
            KeyCode::Char('d') if alt => {
                self.input.kill_word_forward();
                None
            }
            KeyCode::Char('f') if alt => {
                self.input.cursor_word_forward();
                None
            }
            KeyCode::Char('b') if alt => {
                self.input.cursor_word_backward();
                None
            }
            KeyCode::Char('<') if alt => {
                self.input.history_beginning();
                None
            }
            KeyCode::Char('>') if alt => {
                self.input.history_end();
                None
            }

            // Alt+Backspace = kill word backward
            KeyCode::Backspace if alt => {
                self.input.kill_word_backward();
                None
            }

            // Up/Down: history (or scroll with Shift)
            KeyCode::Up if shift => {
                self.scroll.scroll_up();
                None
            }
            KeyCode::Up => {
                self.input.history_prev();
                None
            }
            KeyCode::Down if shift => {
                self.scroll.scroll_down();
                None
            }
            KeyCode::Down => {
                self.input.history_next();
                None
            }

            KeyCode::PageUp => {
                self.scroll.scroll_page_up();
                None
            }
            KeyCode::PageDown => {
                self.scroll.scroll_page_down();
                None
            }
            KeyCode::Home => {
                self.scroll.scroll_to_top();
                None
            }
            KeyCode::End => {
                self.scroll.scroll_to_bottom();
                None
            }

            // Backspace (without Alt — Alt+Backspace handled above)
            KeyCode::Backspace => {
                self.input.backspace();
                None
            }

            // Left/Right: cursor (or scroll with Shift)
            KeyCode::Left if shift => {
                self.scroll.scroll_left();
                None
            }
            KeyCode::Left => {
                self.input.cursor_left();
                None
            }
            KeyCode::Right if shift => {
                self.scroll.scroll_right();
                None
            }
            KeyCode::Right => {
                self.input.cursor_right();
                None
            }

            // Plain char insert (no Ctrl modifier)
            KeyCode::Char(ch) if !ctrl => {
                self.input.insert_char(ch);
                None
            }

            _ => None,
        }
    }

    pub fn handle_bus_message(&mut self, msg: &Message) {
        let msg_type = msg.meta_type().unwrap_or_default().to_string();

        // Filter out noise types
        if matches!(
            msg_type.as_str(),
            "LlmStreamEnd" | "LlmToolCalls" | "StreamStats"
        ) {
            return;
        }

        // Handle state replay (from /load): rebuild chat from saved conversation
        if msg_type == "StateReplay" {
            self.chat.clear();
            self.last_stream_type = None;
            self.last_tool_call_index = None;
            if let Some(messages) = msg.payload.get("messages").and_then(|m| m.as_array()) {
                for replay_msg in messages {
                    self.replay_message_to_app(replay_msg);
                }
            }
            self.scroll.scroll_to_bottom();
            return;
        }

        // Handle streaming tokens (append to last assistant message)
        if matches!(msg_type.as_str(), "LlmStream" | "LlmThinking") {
            let payload = msg.payload_str().unwrap_or_default();
            if !payload.is_empty() {
                if self.chat.message_count() > 0 {
                    let last = self.chat.messages().last().unwrap();
                    if last.role == crate::state::AppMessageRole::Assistant {
                        // Insert newline on thinking→content or content→thinking transition
                        if let Some(ref prev) = self.last_stream_type {
                            if prev.as_str() != msg_type.as_str() {
                                self.chat.append_to_last("\n");
                            }
                        }
                        self.chat.append_to_last(&payload);
                        self.last_stream_type = Some(msg_type);
                        return;
                    }
                }
                self.chat.add_message(AppMessage::assistant(&payload));
                self.last_stream_type = Some(msg_type);
            }
            return;
        }

        // Handle tool call building progress (replace last tool message if same index)
        if msg_type == "LlmToolCall" {
            let call_index = msg
                .payload
                .get("index")
                .and_then(|i| i.as_u64())
                .map(|v| v as usize);
            let name = msg
                .payload
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let args = msg
                .payload
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .unwrap_or("");
            let content = if !name.is_empty() && !args.is_empty() {
                format!("{}({})", name, args)
            } else if !name.is_empty() {
                format!("{}(building...)", name)
            } else {
                "building...".to_string()
            };

            // If same tool call index and last message is Tool, replace it
            if call_index == self.last_tool_call_index && self.chat.message_count() > 0 {
                let last = self.chat.messages().last().unwrap();
                if last.role == crate::state::AppMessageRole::Tool {
                    self.chat.replace_last(content);
                    return;
                }
            }
            // New tool call — add new message
            self.chat.add_message(AppMessage {
                role: crate::state::AppMessageRole::Tool,
                content,
            });
            self.last_tool_call_index = call_index;
            self.last_stream_type = None;
            return;
        }

        // All other message types
        if let Some(app_msg) = bus_to_app_message(msg) {
            self.chat.add_message(app_msg);
            self.last_stream_type = None;
            self.last_tool_call_index = None;
        }
    }

    /// Convert a saved conversation message (LLM API format) to AppMessage(s)
    /// and add them to the chat. Used during StateReplay to reconstruct the
    /// display from the loaded ConversationState.
    fn replay_message_to_app(&mut self, msg: &serde_json::Value) {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "user" => {
                let content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                self.chat.add_message(AppMessage::user(content));
            }
            "assistant" => {
                let reasoning = msg
                    .get("reasoning")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    if let Some(r) = &reasoning {
                        if !r.is_empty() {
                            self.chat.add_message(AppMessage::assistant(r.clone()));
                        }
                    }
                    for tc in tool_calls {
                        let name = tc["function"]["name"].as_str().unwrap_or("");
                        let args = tc["function"]["arguments"].as_str().unwrap_or("");
                        let content = if !name.is_empty() && !args.is_empty() {
                            format!("{}({})", name, args)
                        } else if !name.is_empty() {
                            format!("{}(building...)", name)
                        } else {
                            "building...".to_string()
                        };
                        self.chat.add_message(AppMessage {
                            role: crate::state::AppMessageRole::Tool,
                            content,
                        });
                    }
                } else {
                    let content = msg
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let full_content = if let Some(r) = reasoning {
                        if !r.is_empty() {
                            if content.is_empty() {
                                r
                            } else {
                                format!("{}\n{}", r, content)
                            }
                        } else {
                            content
                        }
                    } else {
                        content
                    };
                    if !full_content.is_empty() {
                        self.chat.add_message(AppMessage::assistant(full_content));
                    }
                }
            }
            "tool" => {
                let name = msg
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_call_id = msg
                    .get("tool_call_id")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let display = format!("[tool_result] {}({}) => {}", name, tool_call_id, content);
                self.chat.add_message(AppMessage {
                    role: crate::state::AppMessageRole::Tool,
                    content: display,
                });
            }
            "system" => {
                let content = msg
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                self.chat.add_message(AppMessage::system(content));
            }
            _ => {}
        }
    }

    pub fn render_frame(&self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
        terminal
            .draw(|frame| {
                self.renderer.render(
                    frame,
                    &self.chat,
                    &self.input,
                    &self.scroll,
                    &self.status,
                );
            })
            .ok();
    }
}

/// Run the TUI app with the bus, reading user input and bus messages concurrently.
/// The user_input_tx channel sends user input text to the agent.
pub async fn run_app(
    bus: Bus,
    user_input_tx: mpsc::UnboundedSender<String>,
    topics: &TopicsConfig,
) -> io::Result<()> {
    let theme = Theme::default_dark();
    let mut app = App::new(theme);

    // Subscribe to the correct bus topics.
    // agent:stream — agent forwards LLM stream output, tool results, and warnings here
    // llm:tool_calls — LLM tool call notifications (which tools the LLM is invoking)
    // llm:stats — stream statistics (token/char counts)
    let topic_names = vec![
        topics.agent_stream.clone(),
        topics.llm_tool_calls.clone(),
        topics.llm_stats.clone(),
        topics.persistence_response.clone(),
    ];

    let mut rx_handles = Vec::new();
    for topic in &topic_names {
        debug!(target: "gladiator-tui", "Subscribing to topic: {}", topic);
        match bus.subscribe_stream(topic).await {
            Ok(rx) => {
                debug!(target: "gladiator-tui", "Subscribed to topic: {}", topic);
                rx_handles.push((topic.clone(), rx));
            }
            Err(e) => {
                tracing::warn!(target: "gladiator-tui", "Failed to subscribe to {}: {:?}", topic, e);
            }
        }
    }

    app.set_status("gladiator ready");

    // Setup terminal
    enable_raw_mode().ok();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).ok();
    execute!(stdout, EnableBracketedPaste).ok();

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend).ok();
    let mut terminal = match terminal {
        Some(t) => t,
        None => {
            disable_raw_mode().ok();
            return Err(io::Error::new(io::ErrorKind::Other, "Failed to create terminal"));
        }
    };

    // Clear screen
    terminal.clear().ok();

    // Track double-ESC for interrupt
    let mut last_esc_time: Option<Instant> = None;

    // Main loop
    let tick = Duration::from_millis(50);
    loop {
        // Render
        app.render_frame(&mut terminal);

        // Check for bus messages (non-blocking)
        for (topic, rx) in rx_handles.iter_mut() {
            while let Ok(msg) = rx.try_recv() {
                let msg_type = msg.meta_type().unwrap_or_default().to_string();
                let payload_preview = msg.payload_str().unwrap_or_default();
                let preview = if payload_preview.len() > 80 {
                    format!("{}...", &payload_preview[..80])
                } else {
                    payload_preview
                };
                debug!(target: "gladiator-tui", "Received on '{}': type={} payload={}", topic, msg_type, preview);
                app.handle_bus_message(&msg);
            }
        }

        // Check for key/paste events (non-blocking with timeout)
        if event::poll(tick).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                match ev {
                    CrosstermEvent::Key(key) => {
                        if key.code == KeyCode::Esc {
                            let now = Instant::now();
                            let should_interrupt = last_esc_time
                                .map(|t| now.duration_since(t) < Duration::from_millis(500))
                                .unwrap_or(false);
                            if should_interrupt {
                                let interrupt_payload = serde_json::json!({
                                    "type": "interrupt",
                                    "reason": "user_stopped"
                                });
                                let msg = Message::new(
                                    &topics.user_control,
                                    "gladiator-tui",
                                    interrupt_payload,
                                );
                                let _ = bus.publish("gladiator-tui", msg).await;
                                app.chat_mut().add_message(AppMessage::system("Stopping inference..."));
                                app.scroll_mut().scroll_to_bottom();
                                app.set_status("Interrupt sent");
                                last_esc_time = None;
                            } else {
                                last_esc_time = Some(now);
                            }
                        } else {
                            if let Some(text) = app.handle_key(key) {
                                if let Some(cmd) = parse_tui_command(&text) {
                                    match cmd {
                                        TuiCommand::Save(filename) => {
                                            let msg = Message::new(
                                                &topics.persistence_command,
                                                "gladiator-tui",
                                                serde_json::json!({"action": "save", "filename": filename, "agent_id": "gladiator-agent-0"}),
                                            );
                                            let _ = bus.publish("gladiator-tui", msg).await;
                                            app.chat_mut().add_message(AppMessage::system(&format!("Saving to {}...", filename)));
                                            app.scroll_mut().scroll_to_bottom();
                                        }
                                        TuiCommand::Load(filename) => {
                                            let msg = Message::new(
                                                &topics.persistence_command,
                                                "gladiator-tui",
                                                serde_json::json!({"action": "load", "filename": filename, "agent_id": "gladiator-agent-0"}),
                                            );
                                            let _ = bus.publish("gladiator-tui", msg).await;
                                            app.chat_mut().add_message(AppMessage::system(&format!("Loading from {}...", filename)));
                                            app.scroll_mut().scroll_to_bottom();
                                        }
                                    }
                                } else {
                                    let _ = user_input_tx.send(text);
                                }
                            }
                        }
                    }
                    CrosstermEvent::Paste(data) => {
                        // Normalize line endings: \r\n → \n, \r → \n
                        let normalized = data.replace("\r\n", "\n").replace("\r", "\n");
                        app.input_mut().insert_str(&normalized);
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit() {
            break;
        }

        sleep(tick).await;
    }

    // Cleanup terminal
    execute!(io::stdout(), DisableBracketedPaste).ok();
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();

    Ok(())
}
