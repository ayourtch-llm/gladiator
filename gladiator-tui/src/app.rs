use crate::commands::{parse_tui_command, TuiCommand};
use crate::event::bus_to_app_message;
use crate::state::{AppMessage, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use crate::render::Renderer;
use gladiator_core::bus::Bus;
use gladiator_core::config::TopicsConfig;
use gladiator_core::message::Message;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    terminal_width: usize,
    pending_messages: Vec<String>,
    is_busy: bool,
    spinner_frame: usize,
    last_spinner_advance: Option<Instant>,
    stream_rx_chars: usize,
    /// Last reported input-token count from StreamStats (for context display).
    ctx_used_tokens: Option<u64>,
    /// Model context window in tokens, when known.
    ctx_window: Option<usize>,
    /// True during the prefill phase (request sent, no token received yet).
    is_prefill: bool,
    /// Number of tool calls dispatched but not yet resolved. When > 0 we
    /// show a "Running tools..." spinner with completed/total counts.
    pending_tool_calls: usize,
    total_tool_calls: usize,

    // --- Thinking ETA estimation ---
    /// Instant when the current (or most recent) LLM request was sent — used
    /// to measure prefill duration and elapsed time for the status display.
    request_start: Option<Instant>,
    /// History of past prefill samples as (input_tokens, duration_ms).
    /// Used to fit a degree-5 polynomial predicting prefill time from token count.
    prefill_history: Vec<(u64, u64)>,
    /// Input tokens for the *current* request if known yet; populated from the
    /// most-recent StreamStats usage.input_tokens. Used as a proxy for current
    /// request size until fresh stats arrive.
    current_request_input_tokens: Option<u64>,
    /// Duration in ms of the prefill phase that just ended (first token arrived).
    /// Paired with input_tokens when StreamStats arrives, then pushed to history.
    pending_prefill_ms: Option<u64>,

    // --- Up/Down navigation mode stickiness ---
    /// When true, plain Up/Down are interpreted as history navigation even if
    /// the buffer is multi-line. Set when an Up/Down key performs a history
    /// action; cleared by any other in-buffer editing/navigation key (Left,
    /// Right, Backspace, char insert, Home/End, etc.) so that subsequent
    /// Up/Down revert to visual line movement.
    up_down_history_mode: bool,

    /// Set when the user presses Up with pending messages and an empty input
    /// buffer — signals run_app to publish a "retrieve_pending" command on the
    /// agent's state_control topic. The agent drains its own pending list and
    /// sends back a RetrievedPending message that populates the editor.
    retract_requested: bool,
}

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
            terminal_width: 80,
            pending_messages: Vec::new(),
            is_busy: false,
            spinner_frame: 0,
            last_spinner_advance: None,
            stream_rx_chars: 0,
            ctx_used_tokens: None,
            ctx_window: None,
            is_prefill: false,
            pending_tool_calls: 0,
            total_tool_calls: 0,
            request_start: None,
            prefill_history: Vec::new(),
            current_request_input_tokens: None,
            pending_prefill_ms: None,
            up_down_history_mode: false,
            retract_requested: false,
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

    /// Force-stop the spinner (e.g. after a user interrupt) so it does not
    /// keep spinning until an LlmStreamEnd bus message arrives.
    pub fn stop_spinner(&mut self) {
        self.is_busy = false;
        self.is_prefill = false;
        self.pending_tool_calls = 0;
        self.total_tool_calls = 0;
        self.spinner_frame = 0;
        self.last_spinner_advance = None;
    }

    /// Reset context-usage tracking. Called when the session is restarted
    /// from file (or otherwise reset), so stale token counts don't linger.
    pub fn reset_context_usage(&mut self) {
        self.ctx_used_tokens = None;
        self.ctx_window = None;
        // Also clear ETA history — a loaded/restarted session has different
        // context characteristics, past timing data is unreliable.
        self.prefill_history.clear();
        self.current_request_input_tokens = None;
        self.request_start = None;
        self.pending_prefill_ms = None;
    }

    /// Advance the spinner frame if busy and ~100ms have elapsed since last advance.
    pub fn tick_spinner(&mut self) {
        if !self.is_busy {
            return;
        }
        let now = Instant::now();
        let due = match self.last_spinner_advance {
            None => true,
            Some(t) => now.duration_since(t).as_millis() >= 100,
        };
        if due {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
            self.last_spinner_advance = Some(now);
        }
    }

    /// Estimate total prefill duration in seconds for the current request,
    /// using a degree-5 polynomial least-squares fit over past samples of
    /// (input_tokens, duration_ms). Returns None if insufficient data.
    ///
    /// Falls back to linear interpolation when fewer than 2 samples exist.
    /// Cache-miss fallback: if elapsed already exceeds 3× the estimate,
    /// return None so only elapsed time is shown instead of a misleading ETA.
    fn estimated_prefill_total_secs(&self) -> Option<u64> {
        let current_tokens = self.current_request_input_tokens.unwrap_or(0);
        // If we don't know token count yet, fall back to average duration.
        if current_tokens == 0 && !self.prefill_history.is_empty() {
            let avg_ms: f64 =
                self.prefill_history.iter().map(|(_, d)| *d as f64).sum::<f64>()
                    / self.prefill_history.len() as f64;
            return Some((avg_ms / 1000.0).max(1.0) as u64);
        }
        if current_tokens == 0 {
            return None;
        }

        let samples: Vec<(f64, f64)> = self
            .prefill_history
            .iter()
            .filter(|(_, d)| *d > 0)
            .map(|(t, d)| (*t as f64, *d as f64))
            .collect();
        if samples.is_empty() {
            return None;
        }

        // Fit polynomial of degree min(5, n-1) via least squares.
        let est_ms = fit_polynomial_predict(&samples, current_tokens as f64);
        let est_secs = (est_ms / 1000.0).max(1.0);

        // Cache-miss fallback: if elapsed already exceeds 3× estimate,
        // show only elapsed time rather than a misleading ETA.
        if let Some(start) = self.request_start {
            let elapsed = start.elapsed().as_secs_f64();
            if est_secs > 0.0 && elapsed > est_secs * 3.0 {
                return None;
            }
        }
        Some(est_secs as u64)
    }

    /// Status string with spinner prefix when busy.
    pub fn display_status(&self) -> String {
        let ctx_part = match (self.ctx_used_tokens, self.ctx_window) {
            (Some(used), Some(win)) if win > 0 => {
                let pct = ((used as f64 / win as f64 * 100.0).min(100.0)) as u64;
                format!(" | ctx {}/{} {}%", used, win, pct)
            }
            (Some(used), None) => format!(" | ctx {} tok", used),
            _ => String::new(),
        };
        if self.is_busy {
            let (label, detail) = if self.pending_tool_calls > 0 {
                ("Running tools...",
                 format!(" {}/{}", self.total_tool_calls - self.pending_tool_calls, self.total_tool_calls))
            } else if self.is_prefill {
                // Thinking... with ETA based on past prefill durations.
                let elapsed_secs = self
                    .request_start
                    .map(|s| s.elapsed().as_secs())
                    .unwrap_or(0);
                match self.estimated_prefill_total_secs() {
                    Some(total) if total > 0 => {
                        let remaining = total.saturating_sub(elapsed_secs);
                        ("Thinking...", format!(" ~{}s left", remaining))
                    }
                    _ => {
                        // No history (or cache-miss fallback): show elapsed only.
                        ("Thinking...", format!(" {}s", elapsed_secs))
                    }
                }
            } else {
                ("Working...", format!("   {} chars", self.stream_rx_chars))
            };
            format!(
                "{} {}{}{}",
                SPINNER_FRAMES[self.spinner_frame], label, detail, ctx_part
            )
        } else {
            let base = self.status.clone();
            if ctx_part.is_empty() { base } else { format!("{} |{}", base, ctx_part) }
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Set the terminal width (columns) used for visual line movement decisions.
    pub fn set_terminal_width(&mut self, width: usize) {
        if width > 0 {
            self.terminal_width = width;
        }
    }

    pub fn terminal_width(&self) -> usize {
        self.terminal_width
    }

    pub fn pending_messages(&self) -> &[String] {
        &self.pending_messages
    }

    pub fn add_pending_message(&mut self, msg: String) {
        self.pending_messages.push(msg);
    }

    /// Called when the agent has processed a user message (either immediately
    /// or after draining from pending). Removes the message from the pending
    /// list (if present) and adds it to the chat as a user message.
    pub fn display_pending_message(&mut self, msg: &str) {
        if let Some(pos) = self.pending_messages.iter().position(|m| m == msg) {
            self.pending_messages.remove(pos);
        }
        self.chat.add_message(AppMessage::user(msg));
        self.scroll.scroll_to_bottom();
    }

    pub fn clear_pending_messages(&mut self) {
        self.pending_messages.clear();
    }

    /// Returns true if a pending-message retraction was requested by the last
    /// key event (Up arrow with non-empty pending list and empty input).
    /// Resets the flag so it's only consumed once.
    pub fn take_retract_request(&mut self) -> bool {
        let v = self.retract_requested;
        self.retract_requested = false;
        v
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
                self.up_down_history_mode = false;
                self.input.insert_newline();
                None
            }
            KeyCode::Enter => {
                let text = self.input.submit();
                self.up_down_history_mode = false;
                if !text.is_empty() {
                    // Don't add to chat locally — the agent will publish
                    // UserMessageDisplayed or UserMessageQueued events
                    // that drive the display.
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

            // Up/Down: visual line movement (if multiline) or history navigation.
            // Shift+Up/Down always scrolls. If up_down_history_mode is sticky
            // (user previously used Up/Down for history), keep treating plain
            // Up/Down as history even in multi-line buffers — until any other
            // editing/navigation key breaks the mode.
            KeyCode::Up if shift => {
                self.scroll.scroll_up();
                None
            }
            KeyCode::Up => {
                // Retract pending messages into the editor for editing:
                // when there are pending messages and the input buffer is empty,
                // Up signals run_app to request "retrieve_pending" from the
                // agent. The agent drains its own pending list (so both sides
                // stay in sync) and sends back a RetrievedPending message that
                // populates the editor for editing.
                if !self.pending_messages.is_empty() && self.input.buffer().is_empty() {
                    self.clear_pending_messages();
                    self.up_down_history_mode = false;
                    self.retract_requested = true;
                    return None;
                }
                let is_multi = self.input.is_multiline(self.terminal_width, InputState::PROMPT_LEN);
                if !is_multi || self.up_down_history_mode {
                    // Single-line: always history. Multi-line + sticky mode:
                    // keep as history until another key breaks the mode.
                    self.input.history_prev();
                    self.up_down_history_mode = true;
                } else {
                    self.input.cursor_up(self.terminal_width, InputState::PROMPT_LEN);
                }
                None
            }
            KeyCode::Down if shift => {
                self.scroll.scroll_down();
                None
            }
            KeyCode::Down => {
                let is_multi = self.input.is_multiline(self.terminal_width, InputState::PROMPT_LEN);
                if !is_multi || self.up_down_history_mode {
                    self.input.history_next();
                    // If we've returned to an empty buffer (history end),
                    // allow the mode to reset so a fresh Up in multi-line
                    // starts doing line navigation again.
                    if self.input.buffer().is_empty() && self.input.cursor() == 0 {
                        self.up_down_history_mode = false;
                    }
                } else {
                    self.input.cursor_down(self.terminal_width, InputState::PROMPT_LEN);
                }
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
            // Home/End inside the input: break sticky history mode and move
            // cursor to line start/end. Shift+Home/End scrolls chat.
            KeyCode::Home if shift => {
                self.scroll.scroll_to_top();
                None
            }
            KeyCode::Home => {
                self.up_down_history_mode = false;
                self.input.cursor_line_start();
                None
            }
            KeyCode::End if shift => {
                self.scroll.scroll_to_bottom();
                None
            }
            KeyCode::End => {
                self.up_down_history_mode = false;
                self.input.cursor_line_end();
                None
            }

            // Backspace (without Alt — Alt+Backspace handled above)
            KeyCode::Backspace => {
                self.up_down_history_mode = false;
                self.input.backspace();
                None
            }

            // Left/Right: cursor (or scroll with Shift). Breaks sticky mode.
            KeyCode::Left if shift => {
                self.scroll.scroll_left();
                None
            }
            KeyCode::Left => {
                self.up_down_history_mode = false;
                self.input.cursor_left();
                None
            }
            KeyCode::Right if shift => {
                self.scroll.scroll_right();
                None
            }
            KeyCode::Right => {
                self.up_down_history_mode = false;
                self.input.cursor_right();
                None
            }

            // Plain char insert (no Ctrl modifier). Breaks sticky mode.
            KeyCode::Char(ch) if !ctrl => {
                self.up_down_history_mode = false;
                self.input.insert_char(ch);
                None
            }

            _ => None,
        }
    }

    pub fn handle_bus_message(&mut self, msg: &Message) {
        let msg_type = msg.meta_type().unwrap_or_default().to_string();

        // Spinner busy tracking:
        //   LlmRequestSent → busy + prefill (Thinking... before any tokens)
        //   LlmStream/LlmThinking → busy, clear prefill
        //   LlmStreamEnd → idle
        match msg_type.as_str() {
            "LlmRequestSent" => {
                self.is_busy = true;
                self.is_prefill = true;
                self.pending_tool_calls = 0;
                self.total_tool_calls = 0;
                self.stream_rx_chars = 0;
                self.spinner_frame = 0;
                self.last_spinner_advance = None;
                // Record the start instant so we can measure prefill duration
                // and show elapsed/ETA in the status bar.
                self.request_start = Some(Instant::now());
            }
            "LlmStream" | "LlmThinking" => {
                if !self.is_busy {
                    self.stream_rx_chars = 0;
                }
                // On the transition prefill → first token, record how long
                // the prefill phase took in ms. We stash it as pending and pair
                // it with input_tokens when StreamStats arrives (StreamStats comes
                // after streaming ends, so this correctly pairs duration of THIS
                // request with its own token count).
                if self.is_prefill {
                    if let Some(start) = self.request_start.take() {
                        let dur_ms = start.elapsed().as_millis() as u64;
                        self.pending_prefill_ms = Some(dur_ms);
                    }
                }
                self.is_busy = true;
                // First token arrives — no longer in prefill.
                self.is_prefill = false;
            }
            "LlmStreamEnd" => {
                self.is_busy = false;
                self.is_prefill = false;
                self.spinner_frame = 0;
                self.last_spinner_advance = None;
            }
            _ => {}
        }

        // Capture context-usage stats from StreamStats (before the noise filter
        // drops them) so we can show "ctx: N/M tok" in the status bar. Also
        // stash input_tokens as a proxy for this request's size — it will be
        // used to estimate ETA on the next prefill.
        if msg_type == "StreamStats" {
            let usage = msg.payload.get("usage");
            let input_tok = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64());
            self.ctx_used_tokens = input_tok;
            // Remember the most recent input-token count for ETA estimation.
            if let Some(tok) = input_tok {
                self.current_request_input_tokens = Some(tok);
                // If we have a pending prefill duration from this request's
                // first-token transition, pair it with the actual token count
                // and push to history. This correctly pairs (tokens, ms).
                if let Some(dur_ms) = self.pending_prefill_ms.take() {
                    self.prefill_history.push((tok, dur_ms));
                    if self.prefill_history.len() > 50 {
                        self.prefill_history.remove(0);
                    }
                }
            }
            self.ctx_window = msg
                .payload
                .get("context_window")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
        }

        // Filter out noise types
        if matches!(
            msg_type.as_str(),
            "LlmStreamEnd" | "LlmToolCalls" | "StreamStats" | "LlmRequestSent"
        ) {
            return;
        }

        // Handle user message queued (pending while agent is busy with tool calls)
        if msg_type == "UserMessageQueued" {
            let text = msg.payload_str().unwrap_or_default();
            if !text.is_empty() {
                self.add_pending_message(text);
            }
            return;
        }

        // Handle user message displayed (agent processed the message, show in chat)
        if msg_type == "UserMessageDisplayed" {
            let text = msg.payload_str().unwrap_or_default();
            if !text.is_empty() {
                self.display_pending_message(&text);
            }
            return;
        }

        // Handle retrieved pending messages (from agent's retrieve_pending
        // state-control command): load the joined text into the input buffer
        // for editing and resubmission.
        if msg_type == "RetrievedPending" {
            let text = msg.payload.get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            self.input.set_buffer(text);
            return;
        }

        // Handle pending user message tracking
        if msg_type == "UserMessageQueued" {
            let text = msg.payload_str().unwrap_or_default();
            if !text.is_empty() {
                self.add_pending_message(text);
            }
            return;
        }
        if msg_type == "UserMessageDisplayed" {
            let text = msg.payload_str().unwrap_or_default();
            if !text.is_empty() {
                self.display_pending_message(&text);
            }
            return;
        }

        // Handle state replay (from /load): rebuild chat from saved conversation
        if msg_type == "StateReplay" {
            self.chat.clear();
            self.pending_messages.clear();
            self.last_stream_type = None;
            self.last_tool_call_index = None;
            self.reset_context_usage();
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
                self.stream_rx_chars += payload.chars().count();
                let is_thinking = msg_type == "LlmThinking";
                let target_role = if is_thinking {
                    crate::state::AppMessageRole::Thinking
                } else {
                    crate::state::AppMessageRole::Assistant
                };
                if self.chat.message_count() > 0 {
                    let last = self.chat.messages().last().unwrap();
                    if last.role == target_role {
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
                if is_thinking {
                    self.chat.add_message(AppMessage::thinking(&payload));
                } else {
                    self.chat.add_message(AppMessage::assistant(&payload));
                }
                self.last_stream_type = Some(msg_type);
            }
            return;
        }

        // Handle tool call building progress — match by stable id (or
        // index-based synthetic key) so multiple concurrent tools in flight
        // each update their own chat line.
        if msg_type == "LlmToolCall" {
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
            // Stable matching key: prefer the LLM-provided call id; fall back
            // to an index-based synthetic key.
            let tool_id = msg.payload.get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    msg.payload.get("index")
                        .and_then(|i| i.as_u64())
                        .map(|idx| format!("__idx_{}", idx))
                });

            // Compute the display content (diff for edit_file/plan_edits, raw
            // args otherwise).
            let parsed_args = serde_json::from_str::<serde_json::Value>(args).ok();
            let content = if !name.is_empty() && !args.is_empty() {
                if let Some(ref p) = parsed_args {
                    if let Some(diff) = crate::diff_render::render_tool_diff(name, p) {
                        format!("{} \n{}", name, diff)
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

            // Match by tool_id: scan backwards for an existing Tool message
            // with the same id and replace it in place (progressive update).
            if let Some(ref tid) = tool_id {
                if let Some(idx) = self.chat.find_tool_by_id(tid) {
                    self.chat.replace_tool(idx, content);
                    return;
                }
            }
            // New tool call — add a new Tool message tagged with the id.
            self.chat.add_message(AppMessage::tool(content, tool_id));
            self.last_stream_type = None;
            return;
        }

        // Track tool-call dispatch/resolution for the "Running tools..." spinner.
        if msg_type == "Info" {
            // Support both legacy plain and structured JSON payloads.
            let is_calling = if let Some(s) = msg.payload_str() {
                s.starts_with("Calling tool:")
            } else if let Some(text) = msg.payload.get("text").and_then(|v| v.as_str()) {
                text.starts_with("Calling tool:")
            } else {
                false
            };
            if is_calling {
                self.pending_tool_calls += 1;
                self.total_tool_calls = self.pending_tool_calls;
                self.is_busy = true;
                self.is_prefill = false;
            }
        }

        // LlmToolResult: one pending call resolved.
        if msg_type == "LlmToolResult" && self.pending_tool_calls > 0 {
            self.pending_tool_calls -= 1;
            if self.pending_tool_calls == 0 {
                self.total_tool_calls = 0;
                self.is_busy = false;
                self.spinner_frame = 0;
                self.last_spinner_advance = None;
            }
        }

        // All other message types
        if let Some(app_msg) = bus_to_app_message(msg) {
            // "Calling tool:" dispatch (structured JSON from agent): the
            // AppMessage returned is Tool-role with tool_id set. Coalesce by
            // matching id back to an existing [tool] placeholder and replace.
            if msg_type == "Info" && app_msg.role == crate::state::AppMessageRole::Tool {
                if let Some(ref tid) = app_msg.tool_id {
                    if let Some(idx) = self.chat.find_tool_by_id(tid) {
                        // Preserve diff rendering: only replace when the
                        // existing [tool] line has no newline (plain name(args)).
                        let existing = &self.chat.messages()[idx];
                        if !existing.content.contains('\n') {
                            self.chat.replace_tool(idx, app_msg.content.clone());
                            return;
                        }
                    }
                }
            }

            // LlmToolResult: coalesce into the matching Tool message by id.
            if msg_type == "LlmToolResult" && app_msg.tool_id.is_some() {
                let tid = app_msg.tool_id.as_ref().unwrap();
                if let Some(idx) = self.chat.find_tool_by_id(tid) {
                    // Append result text after a separator so call + result
                    // show as one coalesced block. Strip the redundant function name
                    // and tool_call_id from the result line since they're already in
                    // the tool-call header above.
                    let existing_content = self.chat.messages()[idx].content.clone();
                    let stripped = strip_tool_result_header(&app_msg.content);
                    self.chat.replace_tool(
                        idx,
                        format!("{}\n  {}", existing_content, stripped),
                    );
                    return;
                }
            }

            self.chat.add_message(app_msg);
            self.last_stream_type = None;
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
                            self.chat.add_message(AppMessage::thinking(r.clone()));
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
                        let tc_id = tc.get("id")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                        self.chat.add_message(AppMessage::tool(content, tc_id));
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
                                self.chat.add_message(AppMessage::thinking(r));
                                return;
                            }
                            self.chat.add_message(AppMessage::thinking(r));
                            if !content.is_empty() {
                                self.chat.add_message(AppMessage::assistant(content));
                            }
                            return;
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
                self.chat.add_message(AppMessage::tool(display, if !tool_call_id.is_empty() { Some(tool_call_id.clone()) } else { None }));
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
        let status = self.display_status();
        terminal
            .draw(|frame| {
                self.renderer.render(
                    frame,
                    &self.chat,
                    &self.input,
                    &self.scroll,
                    &status,
                    &self.pending_messages,
                );
            })
            .ok();
    }
}

/// Fit a least-squares polynomial of degree min(5, n-1) through samples
/// (x=tokens, y=duration_ms), normalized to avoid overflow in high powers.
/// Returns predicted duration_ms at `query_x`.
fn fit_polynomial_predict(samples: &[(f64, f64)], query_x: f64) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    // Linear fallback for small sample sets.
    if n < 2 {
        return samples[0].1.max(1.0);
    }

    // Normalize x by dividing by max_x so powers stay in [0, ~1].
    let max_x = samples.iter().map(|(x, _)| *x).fold(0.0_f64, f64::max);
    if max_x <= 0.0 {
        return samples[0].1.max(1.0);
    }

    // Degree capped at min(5, n-1) — need at least degree+1 points to fit.
    let d = 5usize.min(n - 1).min(samples.len().saturating_sub(1));
    if d == 0 {
        return samples[0].1.max(1.0);
    }

    // Build normal equations: A β = b where
    // A[i][j] = Σ u_k^(i+j), b[j] = Σ y_k * u_j^k, for i,j in 0..=d.
    let sz = d + 1;
    let mut a = vec![vec![0.0_f64; sz]; sz];
    let mut b = vec![0.0_f64; sz];

    for &(x, y) in samples {
        let u = x / max_x;
        // Precompute powers up to 2*d.
        let mut pows = vec![1.0_f64; 2 * d + 1];
        for i in 1..=(2 * d) {
            pows[i] = pows[i - 1] * u;
        }
        for j in 0..sz {
            b[j] += y * pows[j];
            for i in 0..sz {
                a[i][j] += pows[i + j];
            }
        }
    }

    // Solve via Gaussian elimination with partial pivoting.
    let coeffs = match solve_linear_system(&mut a, &b) {
        Some(c) => c,
        None => return samples[0].1.max(1.0),
    };

    // Evaluate polynomial at normalized query point using Horner's method:
    // f(u) = c_0 + u*(c_1 + u*(... ))
    let qu = if max_x > 0.0 { query_x / max_x } else { 0.0 };
    let mut result = 0.0_f64;
    for &c in coeffs.iter().rev() {
        result = result * qu + c;
    }

    // Clamp to non-negative — prefill time can't be negative.
    if result < 1.0 || !result.is_finite() {
        return samples[0].1.max(1.0);
    }
    result
}

/// Solve a linear system A x = b via Gaussian elimination with partial pivoting.
/// Modifies `a` in place (augments with b column). Returns solution or None if singular.
fn solve_linear_system(a: &mut [Vec<f64>], b: &[f64]) -> Option<Vec<f64>> {
    let n = a.len();
    if n == 0 || a.iter().any(|row| row.len() != n) {
        return None;
    }

    // Augmented matrix [A | b]
    for i in 0..n {
        a[i].push(b[i]);
    }
    let ncols = n + 1;

    // Forward elimination with partial pivoting.
    for col in 0..n.saturating_sub(1) {
        // Find pivot row (max absolute value in column).
        let mut max_row = col;
        let mut max_val = a[col][col].abs();
        for r in (col + 1)..n {
            if a[r][col].abs() > max_val {
                max_val = a[r][col].abs();
                max_row = r;
            }
        }

        // Swap rows.
        if max_row != col {
            a.swap(col, max_row);
        }

        let pivot = a[col][col];
        if pivot.abs() < 1e-12 {
            return None; // Singular matrix
        }

        for r in (col + 1)..n {
            let factor = a[r][col] / pivot;
            for c in col..ncols {
                a[r][c] -= factor * a[col][c];
            }
        }
    }

    // Back substitution.
    if n == 0 || a[n - 1][n - 1].abs() < 1e-12 {
        return None;
    }

    let mut x = vec![0.0_f64; n];
    for i in (0..n).rev() {
        let mut sum = a[i][ncols - 1]; // augmented column
        for j in (i + 1)..n {
            sum -= a[i][j] * x[j];
        }
        if a[i][i].abs() < 1e-12 {
            return None;
        }
        x[i] = sum / a[i][i];
    }

    Some(x)
}

/// Run the TUI app with the bus, reading user input and bus messages concurrently.
/// The user_input_tx channel sends user input text to the agent.
pub async fn run_app(
    bus: Bus,
    user_input_tx: mpsc::UnboundedSender<String>,
    topics: &TopicsConfig,
    working_dir: &str,
    debug_flag: Arc<AtomicBool>,
    sandbox_toggle: Arc<AtomicBool>,
) -> io::Result<()> {
    let theme = Theme::default_dark();
    let mut app = App::new(theme);
    let fixme_store = gladiator_tools::FixmeStore::new(working_dir);

    // Subscribe to the correct bus topics.
    // agent:stream — agent forwards LLM stream output, tool results, and warnings here
    // llm:tool_calls — LLM tool call notifications (which tools the LLM is invoking)
    // llm:stats — stream statistics (token/char counts)
    let topic_names = vec![
        topics.agent_stream.clone(),
        topics.llm_tool_calls.clone(),
        topics.llm_stats.clone(),
        topics.persistence_response.clone(),
        topics.log.clone(),
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
        // Update terminal width for visual line movement
        if let Ok((cols, _)) = crossterm::terminal::size() {
            app.set_terminal_width(cols as usize);
        }

        // Render
        app.tick_spinner();
        app.render_frame(&mut terminal);

        // Check for bus messages (non-blocking)
        for (topic, rx) in rx_handles.iter_mut() {
            while let Ok(msg) = rx.try_recv() {
                let msg_type = msg.meta_type().unwrap_or_default().to_string();
                let payload_preview = msg.payload_str().unwrap_or_default();
                let preview = if payload_preview.len() > 80 {
                    let truncated: String = payload_preview.chars().take(80).collect();
                    format!("{}...", truncated)
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
                                app.stop_spinner();
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
                                        TuiCommand::Fixme(phrase) => {
                                            match fixme_store.add(&phrase) {
                                                Ok(entry) => {
                                                    app.chat_mut().add_message(AppMessage::system(
                                                        &format!("Added fixme: {} (id: {})", entry.phrase, entry.id),
                                                    ));
                                                }
                                                Err(e) => {
                                                    app.chat_mut().add_message(AppMessage::error(
                                                        &format!("Failed to add fixme: {}", e),
                                                    ));
                                                }
                                            }
                                            app.scroll_mut().scroll_to_bottom();
                                        }
                                        TuiCommand::Debug(enabled) => {
                                            debug_flag.store(enabled, Ordering::Relaxed);
                                            if enabled {
                                                app.chat_mut().add_message(AppMessage::system(
                                                    "Debug mode enabled — tracing output will appear in chat",
                                                ));
                                                app.set_status("Debug: ON");
                                            } else {
                                                app.chat_mut().add_message(AppMessage::system(
                                                    "Debug mode disabled",
                                                ));
                                                app.set_status("Debug: OFF");
                                            }
                                            app.scroll_mut().scroll_to_bottom();
                                        }
                                        TuiCommand::Sandbox(enabled) => {
                                            sandbox_toggle.store(enabled, Ordering::Relaxed);
                                            if enabled {
                                                app.chat_mut().add_message(AppMessage::system(
                                                    "Sandbox enabled — bash commands run under sandbox-exec",
                                                ));
                                                app.set_status("Sandbox: ON");
                                            } else {
                                                app.chat_mut().add_message(AppMessage::system(
                                                    "Sandbox disabled — bash commands run without sandboxing",
                                                ));
                                                app.set_status("Sandbox: OFF");
                                            }
                                            app.scroll_mut().scroll_to_bottom();
                                        }
                                    }
                                } else {
                                    let _ = user_input_tx.send(text);
                                }
                            } else if app.take_retract_request() {
                                // Up arrow with pending messages: ask the agent
                                // to drain its own pending list and send back a
                                // RetrievedPending message that populates the editor.
                                let msg = Message::new(
                                    &topics.agent_state_control,
                                    "gladiator-tui",
                                    serde_json::json!({"type": "retrieve_pending", "agent_id": "gladiator-agent-0"}),
                                );
                                let _ = bus.publish("gladiator-tui", msg).await;
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

/// Strip the redundant `[tool_result] func_name(tool_call_id)` prefix from a
/// tool result line, leaving just `=> text` (or `(error) => text`).
fn strip_tool_result_header(content: &str) -> String {
    // Input form: "  [tool_result] func_name(id) => rest"
    // or          : "  [tool_error] func_name(id) => rest"
    let trimmed = content.trim();
    if let Some(arrow_pos) = trimmed.find("=>") {
        let after_arrow = &trimmed[arrow_pos..];
        return format!("  {}", after_arrow.trim());
    }
    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polynomial_predict_linear_data() {
        // Perfect linear relationship: ms = 2 * tokens
        let samples = vec![
            (100.0, 200.0),
            (500.0, 1000.0),
            (1000.0, 2000.0),
            (2000.0, 4000.0),
            (3000.0, 6000.0),
            (5000.0, 10000.0),
        ];
        // With degree-1 fit through linear data, prediction should be exact.
        let pred = fit_polynomial_predict(&samples, 2500.0);
        assert!((pred - 5000.0).abs() < 50.0,
            "expected ~5000ms for 2500 tokens, got {}", pred);

        // Edge: zero query
        let _p0 = fit_polynomial_predict(&samples, 0.0);
    }

    #[test]
    fn polynomial_predict_empty_samples() {
        assert_eq!(fit_polynomial_predict(&[], 100.0), 0.0);
    }

    #[test]
    fn polynomial_predict_single_sample() {
        let samples = vec![(500.0, 800.0)];
        // Single sample: should return the single y value
        assert_eq!(fit_polynomial_predict(&samples, 300.0), 800.0_f64.max(1.0));
    }

    #[test]
    fn polynomial_predict_two_samples_linear() {
        let samples = vec![(100.0, 200.0), (500.0, 600.0)];
        // Two points: degree-1 linear fit should interpolate exactly.
        let pred = fit_polynomial_predict(&samples, 300.0);
        assert!((pred - 400.0).abs() < 10.0,
            "expected ~400ms for 300 tokens (linear interp), got {}", pred);
    }

    #[test]
    fn polynomial_predict_six_samples_degree5() {
        // Cubic relationship: ms = 3 * t^2 + small noise
        let samples: Vec<(f64, f64)> = vec![
            (100.0, 30000.0),
            (200.0, 120000.0),
            (400.0, 480000.0),
            (800.0, 1920000.0),
            (1600.0, 7680000.0),
            (3200.0, 30720000.0),
        ];
        // With degree-5 fit on perfectly quadratic data, prediction should
        // be close to the true value at an intermediate point.
        let pred = fit_polynomial_predict(&samples, 600.0);
        assert!(pred > 500_000.0 && pred < 1_200_000.0,
            "expected ~108000ms for 600 tokens (quadratic), got {}", pred);
    }

    #[test]
    fn solve_linear_system_basic() {
        // Simple 2x2: x + y = 3, 2x - y = 6 => x=3, y=0
        let mut a = vec![vec![1.0_f64, 1.0], vec![2.0, -1.0]];
        let b = vec![3.0_f64, 6.0];
        let result = solve_linear_system(&mut a, &b);
        assert!(result.is_some());
        let x = result.unwrap();
        assert!((x[0] - 3.0).abs() < 1e-10, "expected x=3, got {}", x[0]);
        assert!((x[1]).abs() < 1e-10, "expected y≈0, got {}", x[1]);
    }

    #[test]
    fn solve_linear_system_singular() {
        // Singular matrix (two identical rows)
        let mut a = vec![vec![2.0_f64; 3], vec![2.0; 3]];
        let b = vec![5.0_f64, 5.0];
        assert!(solve_linear_system(&mut a, &b).is_none());
    }
}

