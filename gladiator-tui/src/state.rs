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
    kill_ring: Vec<String>,
    last_op_was_kill: bool,
}

/// Information about a single visual line within the input buffer.
#[derive(Debug, Clone)]
pub struct VisualLineInfo {
    /// Index of the logical line (0-based, split by '\n') this visual line belongs to.
    pub logical_line: usize,
    /// Char offset within the logical line where this visual line starts.
    pub char_start: usize,
    /// Number of chars in this visual line.
    pub char_count: usize,
}

/// Visual layout of the input buffer for a given terminal width.
#[derive(Debug, Clone)]
pub struct VisualLayout {
    /// All visual lines, in order.
    pub visual_lines: Vec<VisualLineInfo>,
    /// Index into visual_lines where the cursor is located.
    pub cursor_visual_line: usize,
    /// Column (char offset) within the cursor's visual line.
    pub cursor_visual_col: usize,
}

impl InputState {
    /// The prompt prefix length in chars ("> " = 2 chars).
    /// Used for visual line calculations (hard-wrapping the first line).
    pub const PROMPT_LEN: usize = 2;
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            kill_ring: Vec::new(),
            last_op_was_kill: false,
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
        self.break_kill_chain();
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn insert_str(&mut self, s: &str) {
        self.break_kill_chain();
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        self.break_kill_chain();
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
        self.break_kill_chain();
        if self.cursor > 0 {
            let mut pos = self.cursor - 1;
            while pos > 0 && !self.buffer.is_char_boundary(pos) {
                pos -= 1;
            }
            self.cursor = pos;
        }
    }

    pub fn cursor_right(&mut self) {
        self.break_kill_chain();
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
        self.break_kill_chain();
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
        self.break_kill_chain();
        let suffix = &self.buffer[self.cursor..];
        if let Some(pos) = suffix.find('\n') {
            self.cursor += pos;
        } else {
            self.cursor = self.buffer.len();
        }
    }

    /// Returns true if the buffer occupies more than one visual line at the
    /// given terminal width (accounting for hard-wrapping and prompt prefix).
    /// Used to decide whether Up/Down should do visual line movement vs
    /// history navigation.
    pub fn is_multiline(&self, width: usize, prompt_len: usize) -> bool {
        let layout = self.visual_layout(width, prompt_len);
        layout.visual_lines.len() > 1
    }

    /// Compute the visual layout of the buffer for a given terminal width.
    ///
    /// `prompt_len` is the number of chars the prompt prefix occupies on the
    /// first logical line (e.g. 2 for "> "). `width` is the available column
    /// width for the input area.
    ///
    /// Returns the full list of visual lines and the cursor's position within
    /// that visual-line grid.
    pub fn visual_layout(&self, width: usize, prompt_len: usize) -> VisualLayout {
        let buffer = &self.buffer;
        let cursor_pos = self.cursor;
        let logical_lines: Vec<&str> = buffer.split('\n').collect();

        // Find which logical line the cursor is on, and the char offset within it.
        let mut cursor_logical_line = 0usize;
        let mut cursor_byte_in_line = 0usize;
        let mut byte_count = 0usize;
        for (i, line) in logical_lines.iter().enumerate() {
            let line_len = line.len();
            if cursor_pos <= byte_count + line_len {
                cursor_logical_line = i;
                cursor_byte_in_line = cursor_pos - byte_count;
                break;
            }
            byte_count += line_len + 1; // +1 for '\n'
            cursor_logical_line = i;
            cursor_byte_in_line = line_len;
        }
        // Edge case: cursor at very end of buffer
        if cursor_pos == buffer.len() {
            cursor_logical_line = logical_lines.len() - 1;
            cursor_byte_in_line = logical_lines[cursor_logical_line].len();
        }

        // Convert byte offset to char offset within the logical line
        let cursor_line_str = logical_lines[cursor_logical_line];
        let cursor_char_in_line = cursor_line_str[..cursor_byte_in_line].chars().count();

        let mut visual_lines: Vec<VisualLineInfo> = Vec::new();
        let mut cursor_visual_line = 0usize;
        let mut cursor_visual_col = 0usize;
        let mut found_cursor = false;

        for (i, line) in logical_lines.iter().enumerate() {
            let avail = if i == 0 {
                width.saturating_sub(prompt_len).max(1)
            } else {
                width.max(1)
            };
            let chars: Vec<char> = line.chars().collect();
            let line_char_count = chars.len();
            let mut pos = 0usize;
            loop {
                let end = (pos + avail).min(line_char_count);
                let chunk_len = end - pos;

                // Check if cursor is on this visual line
                if !found_cursor && i == cursor_logical_line {
                    if cursor_char_in_line >= pos && cursor_char_in_line < end {
                        cursor_visual_line = visual_lines.len();
                        cursor_visual_col = cursor_char_in_line - pos;
                        found_cursor = true;
                    } else if cursor_char_in_line == end && end >= line_char_count {
                        // Cursor at end of logical line
                        cursor_visual_line = visual_lines.len();
                        cursor_visual_col = end - pos;
                        found_cursor = true;
                    }
                    // else: cursor at wrap boundary — will be found on next visual line
                }

                visual_lines.push(VisualLineInfo {
                    logical_line: i,
                    char_start: pos,
                    char_count: chunk_len,
                });

                if end >= line_char_count {
                    break;
                }
                pos = end;
            }
        }

        // Fallback: cursor not placed
        if !found_cursor {
            cursor_visual_line = visual_lines.len().saturating_sub(1);
            cursor_visual_col = visual_lines
                .last()
                .map(|v| v.char_count)
                .unwrap_or(0);
        }

        if visual_lines.is_empty() {
            visual_lines.push(VisualLineInfo {
                logical_line: 0,
                char_start: 0,
                char_count: 0,
            });
            cursor_visual_line = 0;
            cursor_visual_col = 0;
        }

        VisualLayout {
            visual_lines,
            cursor_visual_line,
            cursor_visual_col,
        }
    }

    /// Move cursor up one visual line. If at the top, does nothing (stays).
    /// Requires `width` and `prompt_len` to compute the visual layout.
    pub fn cursor_up(&mut self, width: usize, prompt_len: usize) {
        self.break_kill_chain();
        let layout = self.visual_layout(width, prompt_len);
        if layout.cursor_visual_line == 0 {
            return; // already at top
        }
        let target_visual = layout.cursor_visual_line - 1;
        let target_info = &layout.visual_lines[target_visual];
        let target_logical = target_info.logical_line;

        // Compute the byte offset of the target visual line's start within the buffer
        let logical_lines: Vec<&str> = self.buffer.split('\n').collect();
        let mut byte_offset_to_logical = 0usize;
        for (i, line) in logical_lines.iter().enumerate() {
            if i == target_logical {
                break;
            }
            byte_offset_to_logical += line.len() + 1; // +1 for '\n'
        }

        let logical_str = logical_lines[target_logical];

        // Target column: keep the visual column if it fits, otherwise clamp to end of target line
        let target_col = layout.cursor_visual_col.min(target_info.char_count);
        let mut byte_in_line = 0usize;
        let mut col = 0usize;
        for ch in logical_str.chars() {
            if col >= target_col {
                break;
            }
            byte_in_line += ch.len_utf8();
            col += 1;
        }

        self.cursor = byte_offset_to_logical + byte_in_line;
    }

    /// Move cursor down one visual line. If at the bottom, does nothing (stays).
    /// Requires `width` and `prompt_len` to compute the visual layout.
    pub fn cursor_down(&mut self, width: usize, prompt_len: usize) {
        self.break_kill_chain();
        let layout = self.visual_layout(width, prompt_len);
        if layout.cursor_visual_line + 1 >= layout.visual_lines.len() {
            return; // already at bottom
        }
        let target_visual = layout.cursor_visual_line + 1;
        let target_info = &layout.visual_lines[target_visual];
        let target_logical = target_info.logical_line;

        // Compute the byte offset of the target visual line's start within the buffer
        let logical_lines: Vec<&str> = self.buffer.split('\n').collect();
        let mut byte_offset_to_logical = 0usize;
        for (i, line) in logical_lines.iter().enumerate() {
            if i == target_logical {
                break;
            }
            byte_offset_to_logical += line.len() + 1; // +1 for '\n'
        }

        let logical_str = logical_lines[target_logical];

        // Target column: keep the visual column if it fits, otherwise clamp to end of target line
        let target_col = layout.cursor_visual_col.min(target_info.char_count);
        let mut byte_in_line = 0usize;
        let mut col = 0usize;
        for ch in logical_str.chars() {
            if col >= target_col {
                break;
            }
            byte_in_line += ch.len_utf8();
            col += 1;
        }

        self.cursor = byte_offset_to_logical + byte_in_line;
    }

    pub fn clear(&mut self) {
        self.break_kill_chain();
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    pub fn submit(&mut self) -> String {
        self.break_kill_chain();
        let text = self.buffer.clone();
        if !text.is_empty() {
            self.history.push(text.clone());
        }
        self.clear();
        text
    }

    /// Navigate to previous entry in history. Replaces current buffer.
    pub fn history_prev(&mut self) {
        self.break_kill_chain();
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
        self.break_kill_chain();
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

    // --- Kill ring accessor ---

    pub fn kill_ring(&self) -> &[String] {
        &self.kill_ring
    }

    // --- Kill ring helpers ---

    /// Break the kill chain. Called at the start of every non-kill mutating op.
    fn break_kill_chain(&mut self) {
        self.last_op_was_kill = false;
    }

    /// Record a forward kill: append killed text to last entry if chain active,
    /// otherwise push a new entry.
    fn record_kill_forward(&mut self, killed: &str) {
        if self.last_op_was_kill && !self.kill_ring.is_empty() {
            if let Some(last) = self.kill_ring.last_mut() {
                last.push_str(killed);
            }
        } else {
            self.kill_ring.push(killed.to_string());
        }
        self.last_op_was_kill = true;
    }

    /// Record a backward kill: prepend killed text to last entry if chain active,
    /// otherwise push a new entry.
    fn record_kill_backward(&mut self, killed: &str) {
        if self.last_op_was_kill && !self.kill_ring.is_empty() {
            if let Some(last) = self.kill_ring.last_mut() {
                last.insert_str(0, killed);
            }
        } else {
            self.kill_ring.push(killed.to_string());
        }
        self.last_op_was_kill = true;
    }

    // --- Word movement (M-f / M-b) ---

    /// Move cursor forward one word (Emacs M-f).
    /// Skip non-word chars, then skip word chars.
    pub fn cursor_word_forward(&mut self) {
        self.break_kill_chain();
        let bytes = self.buffer.as_bytes();
        let len = self.buffer.len();
        let mut pos = self.cursor;
        // Skip non-word chars
        while pos < len && !is_word_byte(bytes[pos]) {
            pos += 1;
        }
        // Skip word chars
        while pos < len && is_word_byte(bytes[pos]) {
            pos += 1;
        }
        self.cursor = pos;
    }

    /// Move cursor backward one word (Emacs M-b).
    /// Skip non-word chars backward, then skip word chars backward.
    pub fn cursor_word_backward(&mut self) {
        self.break_kill_chain();
        let bytes = self.buffer.as_bytes();
        let mut pos = self.cursor;
        // Skip non-word chars backward
        while pos > 0 && !is_word_byte(bytes[pos - 1]) {
            pos -= 1;
        }
        // Skip word chars backward
        while pos > 0 && is_word_byte(bytes[pos - 1]) {
            pos -= 1;
        }
        self.cursor = pos;
    }

    // --- Deletion ---

    /// Delete char at cursor (forward). C-d. No kill ring.
    pub fn delete_char_forward(&mut self) {
        self.break_kill_chain();
        if self.cursor < self.buffer.len() {
            // Find the char boundary at cursor
            let mut pos = self.cursor;
            while pos < self.buffer.len() && !self.buffer.is_char_boundary(pos) {
                pos += 1;
            }
            if pos < self.buffer.len() {
                self.buffer.remove(pos);
            }
        }
    }

    /// Kill from cursor to end of line (C-k). Does not cross newline.
    pub fn kill_to_end_of_line(&mut self) {
        let line_end = self.line_end_pos();
        if self.cursor < line_end {
            let killed: String = self.buffer.drain(self.cursor..line_end).collect();
            self.record_kill_forward(&killed);
        } else {
            self.break_kill_chain();
        }
    }

    /// Kill from start of line to cursor (C-u). Does not cross newline.
    pub fn kill_to_start_of_line(&mut self) {
        let line_start = self.line_start_pos();
        if self.cursor > line_start {
            let killed: String = self.buffer.drain(line_start..self.cursor).collect();
            self.cursor = line_start;
            self.record_kill_backward(&killed);
        } else {
            self.break_kill_chain();
        }
    }

    /// Kill word forward (M-d). Pushes killed text to kill ring.
    pub fn kill_word_forward(&mut self) {
        let bytes = self.buffer.as_bytes();
        let len = self.buffer.len();
        let mut pos = self.cursor;
        // Skip non-word chars
        while pos < len && !is_word_byte(bytes[pos]) {
            pos += 1;
        }
        // Skip word chars
        while pos < len && is_word_byte(bytes[pos]) {
            pos += 1;
        }
        if pos > self.cursor {
            let killed: String = self.buffer.drain(self.cursor..pos).collect();
            self.record_kill_forward(&killed);
        } else {
            self.break_kill_chain();
        }
    }

    /// Kill word backward (M-Backspace / C-w). Pushes killed text to kill ring.
    pub fn kill_word_backward(&mut self) {
        let bytes = self.buffer.as_bytes();
        let mut pos = self.cursor;
        // Skip non-word chars backward
        while pos > 0 && !is_word_byte(bytes[pos - 1]) {
            pos -= 1;
        }
        // Skip word chars backward
        while pos > 0 && is_word_byte(bytes[pos - 1]) {
            pos -= 1;
        }
        if pos < self.cursor {
            let killed: String = self.buffer.drain(pos..self.cursor).collect();
            self.cursor = pos;
            self.record_kill_backward(&killed);
        } else {
            self.break_kill_chain();
        }
    }

    // --- Yank (C-y) ---

    /// Yank: paste most recent kill-ring entry at cursor.
    pub fn yank(&mut self) {
        self.break_kill_chain();
        if let Some(killed) = self.kill_ring.last() {
            let text = killed.clone();
            self.buffer.insert_str(self.cursor, &text);
            self.cursor += text.len();
        }
    }

    // --- History: M-< / M-> ---

    /// Jump to oldest history entry (M-<).
    pub fn history_beginning(&mut self) {
        self.break_kill_chain();
        if self.history.is_empty() {
            return;
        }
        self.history_index = Some(0);
        self.buffer = self.history[0].clone();
        self.cursor = self.buffer.len();
    }

    /// Jump to newest / empty input (M->).
    pub fn history_end(&mut self) {
        self.break_kill_chain();
        self.history_index = None;
        self.buffer.clear();
        self.cursor = 0;
    }

    // --- Private helpers for line positions ---

    /// Position of the start of the current line (after previous newline or 0).
    fn line_start_pos(&self) -> usize {
        let prefix = &self.buffer[..self.cursor];
        if let Some(pos) = prefix.rfind('\n') {
            pos + 1
        } else {
            0
        }
    }

    /// Position of the end of the current line (at next newline or buffer end).
    fn line_end_pos(&self) -> usize {
        let suffix = &self.buffer[self.cursor..];
        if let Some(pos) = suffix.find('\n') {
            self.cursor + pos
        } else {
            self.buffer.len()
        }
    }
}

/// Word-char predicate (byte-level). Alphanumeric (ASCII) only.
/// Matches test expectations for `foo.bar baz` etc.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
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

    pub fn total_lines(&self) -> usize {
        self.total_lines.get()
    }

    pub fn visible_height(&self) -> usize {
        self.visible_height.get()
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
