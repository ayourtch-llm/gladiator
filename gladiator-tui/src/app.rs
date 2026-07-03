use crate::event::bus_to_app_message;
use crate::state::{AppMessage, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use crate::render::Renderer;
use gladiator_core::bus::Bus;
use gladiator_core::config::TopicsConfig;
use gladiator_core::message::Message;
use std::io;
use std::time::Duration;
use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
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
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                self.quit();
                None
            }
            KeyCode::Char('q') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                self.quit();
                None
            }
            KeyCode::Enter => {
                let text = self.input.submit();
                if !text.is_empty() {
                    self.chat.add_message(AppMessage::user(&text));
                    Some(text)
                } else {
                    None
                }
            }
            KeyCode::Backspace => {
                self.input.backspace();
                None
            }
            KeyCode::Left => {
                self.input.cursor_left();
                None
            }
            KeyCode::Right => {
                self.input.cursor_right();
                None
            }
            KeyCode::Char(ch) => {
                self.input.insert_char(ch);
                None
            }
            _ => None,
        }
    }

    pub fn handle_bus_message(&mut self, msg: &Message) {
        if let Some(app_msg) = bus_to_app_message(msg) {
            // For streaming tokens, append to last assistant message
            let msg_type = msg.meta_type();
            let is_stream = matches!(msg_type, Some("LlmStream") | Some("LlmThinking"));
            if is_stream {
                let payload = msg.payload_str().unwrap_or_default();
                if self.chat.message_count() > 0 {
                    let last = self.chat.messages().last().unwrap();
                    if last.role == crate::state::AppMessageRole::Assistant {
                        self.chat.append_to_last(&payload);
                        return;
                    }
                }
                self.chat.add_message(AppMessage::assistant(&payload));
            } else {
                self.chat.add_message(app_msg);
            }
            // auto-scroll to bottom
            self.scroll.scroll_to_bottom(self.chat.message_count(), 0);
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

        // Check for key events (non-blocking with timeout)
        if event::poll(tick).unwrap_or(false) {
            if let Ok(CrosstermEvent::Key(key)) = event::read() {
                if let Some(text) = app.handle_key(key) {
                    let _ = user_input_tx.send(text);
                }
            }
        }

        if app.should_quit() {
            break;
        }

        sleep(tick).await;
    }

    // Cleanup terminal
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();

    Ok(())
}
