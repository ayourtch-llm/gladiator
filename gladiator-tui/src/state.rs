#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMessageRole {
    User,
    Assistant,
    Tool,
    Error,
    Info,
    System,
}

#[derive(Debug, Clone)]
pub struct AppMessage {
    pub role: AppMessageRole,
    pub content: String,
}

impl AppMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: AppMessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: AppMessageRole::Assistant,
            content: content.into(),
        }
    }

    pub fn tool_call(tool_name: &str, args: &str, result: &str) -> Self {
        Self {
            role: AppMessageRole::Tool,
            content: format!("[{}] {} => {}", tool_name, args, result),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            role: AppMessageRole::Error,
            content: content.into(),
        }
    }

    pub fn info(content: impl Into<String>) -> Self {
        Self {
            role: AppMessageRole::Info,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: AppMessageRole::System,
            content: content.into(),
        }
    }
}

#[derive(Debug)]
pub struct InputState {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find previous char boundary
            let mut pos = self.cursor - 1;
            while pos > 0 && !self.buffer.is_char_boundary(pos) {
                pos -= 1;
            }
            self.buffer.remove(pos);
            self.cursor = pos;
        }
    }

    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            let mut pos = self.cursor - 1;
            while pos > 0 && !self.buffer.is_char_boundary(pos) {
                pos -= 1;
            }
            self.cursor = pos;
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor < self.buffer.len() {
            let mut pos = self.cursor + 1;
            while pos < self.buffer.len() && !self.buffer.is_char_boundary(pos) {
                pos += 1;
            }
            self.cursor = pos;
        }
    }

    /// Move cursor to beginning of line (Home key within multi-line input).
    pub fn cursor_line_start(&mut self) {
        // Find the previous newline before cursor
        let prefix = &self.buffer[..self.cursor];
        if let Some(pos) = prefix.rfind('\n') {
            self.cursor = pos + 1;
        } else {
            self.cursor = 0;
        }
    }

    /// Move cursor to end of line (End key within multi-line input).
    pub fn cursor_line_end(&mut self) {
        let suffix = &self.buffer[self.cursor..];
        if let Some(pos) = suffix.find('\n') {
            self.cursor += pos;
        } else {
            self.cursor = self.buffer.len();
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    pub fn submit(&mut self) -> String {
        let text = self.buffer.clone();
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        self.clear();
        text
    }

    /// Navigate to previous entry in history. Replaces current buffer.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_index = Some(self.history.len() - 1);
            }
            Some(i) => {
                if i > 0 {
                    self.history_index = Some(i - 1);
                }
            }
        }
        if let Some(i) = self.history_index {
            self.buffer = self.history[i].clone();
            self.cursor = self.buffer.len();
        }
    }

    /// Navigate to next entry in history. Clears buffer at the end.
    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(i) => {
                if i + 1 < self.history.len() {
                    self.history_index = Some(i + 1);
                    self.buffer = self.history[i + 1].clone();
                    self.cursor = self.buffer.len();
                } else {
                    self.history_index = None;
                    self.buffer.clear();
                    self.cursor = 0;
                }
            }
        }
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

use std::cell::Cell;

#[derive(Debug)]
pub struct ScrollState {
    offset: Cell<usize>,
    h_offset: Cell<usize>,
    visible_height: Cell<usize>,
    total_lines: Cell<usize>,
    stick_to_bottom: Cell<bool>,
}

impl ScrollState {
    pub fn new() -> Self {
        Self {
            offset: Cell::new(0),
            h_offset: Cell::new(0),
            visible_height: Cell::new(0),
            total_lines: Cell::new(0),
            stick_to_bottom: Cell::new(true),
        }
    }

    pub fn offset(&self) -> usize {
        self.offset.get()
    }

    pub fn h_offset(&self) -> usize {
        self.h_offset.get()
    }

    pub fn stick_to_bottom(&self) -> bool {
        self.stick_to_bottom.get()
    }

    /// Set the visible height (called by renderer).
    pub fn set_visible_height(&self, h: usize) {
        self.visible_height.set(h);
    }

    /// Set the total line count (called by renderer).
    pub fn set_total_lines(&self, n: usize) {
        self.total_lines.set(n);
    }

    /// Maximum scroll offset (total_lines - visible_height).
    pub fn max_offset(&self) -> usize {
        self.total_lines
            .get()
            .saturating_sub(self.visible_height.get())
    }

    /// If stick_to_bottom is true, snap offset to max_offset and reset h_offset.
    /// Called by renderer after setting total_lines/visible_height.
    pub fn update_if_sticking(&self) {
        if self.stick_to_bottom.get() {
            self.offset.set(self.max_offset());
            self.h_offset.set(0);
        }
    }

    pub fn scroll_up(&mut self) {
        self.stick_to_bottom.set(false);
        let cur = self.offset.get();
        if cur > 0 {
            self.offset.set(cur - 1);
        }
    }

    pub fn scroll_down(&mut self) {
        let max = self.max_offset();
        let cur = self.offset.get();
        if cur < max {
            self.offset.set(cur + 1);
            if cur + 1 >= max {
                self.stick_to_bottom.set(true);
            }
        } else {
            self.stick_to_bottom.set(true);
        }
    }

    pub fn scroll_left(&mut self) {
        let cur = self.h_offset.get();
        if cur > 0 {
            self.h_offset.set(cur - 1);
        }
    }

    pub fn scroll_right(&mut self) {
        let cur = self.h_offset.get();
        self.h_offset.set(cur + 1);
    }

    pub fn scroll_page_up(&mut self) {
        self.stick_to_bottom.set(false);
        let cur = self.offset.get();
        let vh = self.visible_height.get().max(1);
        self.offset.set(cur.saturating_sub(vh));
    }

    pub fn scroll_page_down(&mut self) {
        let max = self.max_offset();
        let cur = self.offset.get();
        let vh = self.visible_height.get().max(1);
        let new_offset = (cur + vh).min(max);
        self.offset.set(new_offset);
        if new_offset >= max {
            self.stick_to_bottom.set(true);
        }
    }

    pub fn scroll_to_top(&mut self) {
        self.offset.set(0);
        self.h_offset.set(0);
        self.stick_to_bottom.set(false);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.stick_to_bottom.set(true);
        self.h_offset.set(0);
        self.offset.set(self.max_offset());
    }
}

impl Default for ScrollState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ChatState {
    messages: Vec<AppMessage>,
}

impl ChatState {
    pub fn new() -> Self {
        Self { messages: Vec::new() }
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn message(&self, index: usize) -> Option<&AppMessage> {
        self.messages.get(index)
    }

    pub fn messages(&self) -> &[AppMessage] {
        &self.messages
    }

    pub fn add_message(&mut self, msg: AppMessage) {
        self.messages.push(msg);
    }

    pub fn append_to_last(&mut self, text: &str) {
        if let Some(last) = self.messages.last_mut() {
            last.content.push_str(text);
        } else {
            self.messages.push(AppMessage::assistant(text));
        }
    }

    /// Replace the content of the last message.
    pub fn replace_last(&mut self, content: String) {
        if let Some(last) = self.messages.last_mut() {
            last.content = content;
        }
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
