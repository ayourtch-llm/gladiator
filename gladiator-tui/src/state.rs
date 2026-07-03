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
}

impl InputState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    pub fn submit(&mut self) -> String {
        let text = self.buffer.clone();
        self.clear();
        text
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct ScrollState {
    offset: usize,
    max_visible: usize,
}

impl ScrollState {
    pub fn new() -> Self {
        Self {
            offset: 0,
            max_visible: 0,
        }
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn max_visible(&self) -> usize {
        self.max_visible
    }

    pub fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }

    pub fn set_max_visible(&mut self, max: usize) {
        self.max_visible = max;
    }

    pub fn scroll_down(&mut self, total: usize, visible: usize) {
        if total > visible {
            let max_offset = total - visible;
            if self.offset < max_offset {
                self.offset += 1;
            }
        }
    }

    pub fn scroll_up(&mut self) {
        if self.offset > 0 {
            self.offset -= 1;
        }
    }

    pub fn scroll_to_bottom(&mut self, total: usize, visible: usize) {
        if total > visible {
            self.offset = total - visible;
        } else {
            self.offset = 0;
        }
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

    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

impl Default for ChatState {
    fn default() -> Self {
        Self::new()
    }
}
