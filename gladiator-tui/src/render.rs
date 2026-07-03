use crate::state::{AppMessage, AppMessageRole, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Word-wrap text into visual lines, preserving newlines.
/// `first_width` is the available width for the first visual line (after prefix).
/// `cont_width` is the available width for all subsequent visual lines.
/// Newlines in the content produce line breaks; each sub-line is word-wrapped.
fn wrap_text(content: &str, first_width: usize, cont_width: usize) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    for line in content.split('\n') {
        if line.is_empty() || line.chars().all(char::is_whitespace) {
            // Empty line from \n - preserve as blank visual line
            result.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_len = 0usize;
        let mut current_width = 0usize;

        for word in line.split_whitespace() {
            let word_len = word.chars().count();

            if current.is_empty() {
                // Starting a new visual line
                current_width = if result.is_empty() {
                    first_width
                } else {
                    cont_width
                };

                if word_len > current_width && current_width > 0 {
                    // Word longer than available width - hard-wrap
                    let chars: Vec<char> = word.chars().collect();
                    let mut start = 0;
                    while start < chars.len() {
                        let w = if result.is_empty() { first_width } else { cont_width };
                        let end = (start + w).min(chars.len());
                        result.push(chars[start..end].iter().collect());
                        start = end;
                    }
                    // current stays empty
                } else {
                    current = word.to_string();
                    current_len = word_len;
                }
            } else if current_len + 1 + word_len > current_width {
                // Word doesn't fit on current line - wrap
                result.push(std::mem::take(&mut current));
                current_len = 0;

                if word_len > cont_width && cont_width > 0 {
                    // Word too long for a full line - hard-wrap
                    let chars: Vec<char> = word.chars().collect();
                    let mut start = 0;
                    while start < chars.len() {
                        let end = (start + cont_width).min(chars.len());
                        result.push(chars[start..end].iter().collect());
                        start = end;
                    }
                } else {
                    current = word.to_string();
                    current_len = word_len;
                    current_width = cont_width;
                }
            } else {
                // Word fits on current line
                current.push(' ');
                current.push_str(word);
                current_len += 1 + word_len;
            }
        }

        // Push remaining current (if non-empty)
        if !current.is_empty() {
            result.push(current);
        }
    }

    if result.is_empty() {
        result.push(String::new());
    }

    result
}

pub struct Renderer {
    theme: Theme,
}

impl Renderer {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn render(
        &self,
        frame: &mut Frame,
        chat: &ChatState,
        input: &InputState,
        scroll: &ScrollState,
        status_text: &str,
    ) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_messages(frame, chunks[1], chat, scroll);
        self.render_input(frame, chunks[2], input);
        self.render_status_bar(frame, chunks[3], status_text);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = "gladiator";
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(self.theme.color_border()))
            .style(Style::default().bg(self.theme.color_background()));

        let para = Paragraph::new(format!(" {} ", title))
            .style(
                Style::default()
                    .fg(self.theme.color_primary())
                    .add_modifier(Modifier::BOLD),
            )
            .block(block);

        frame.render_widget(para, area);
    }

    fn render_messages(
        &self,
        frame: &mut Frame,
        area: Rect,
        chat: &ChatState,
        scroll: &ScrollState,
    ) {
        let block = Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(self.theme.color_background()));

        let messages = chat.messages();
        let visible_height = area.height as usize;
        let width = area.width as usize;

        // Build a flat list of visual lines by wrapping each message
        let mut all_visual_lines: Vec<Line> = Vec::new();
        for msg in messages {
            let lines = self.message_to_visual_lines(msg, width);
            all_visual_lines.extend(lines);
        }

        let total_lines = all_visual_lines.len();

        // Update scroll state with current dimensions
        scroll.set_total_lines(total_lines);
        scroll.set_visible_height(visible_height);
        scroll.update_if_sticking();

        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = scroll.offset().min(max_offset);

        let visible: Vec<Line> = all_visual_lines
            .into_iter()
            .skip(offset)
            .take(visible_height)
            .collect();

        let para = Paragraph::new(visible).block(block);
        frame.render_widget(para, area);
    }

    fn message_to_visual_lines(&self, msg: &AppMessage, width: usize) -> Vec<Line<'_>> {
        let (prefix, prefix_color, text_color) = match msg.role {
            AppMessageRole::User => (">", self.theme.color_secondary(), self.theme.color_text()),
            AppMessageRole::Assistant => ("", self.theme.color_primary(), self.theme.color_text()),
            AppMessageRole::Tool => ("[tool]", self.theme.color_info(), self.theme.color_info()),
            AppMessageRole::Error => ("[!]", self.theme.color_error(), self.theme.color_error()),
            AppMessageRole::Info => ("[i]", self.theme.color_info(), self.theme.color_text_muted()),
            AppMessageRole::System => ("[sys]", self.theme.color_text_muted(), self.theme.color_text_muted()),
        };

        let prefix_str = if !prefix.is_empty() {
            format!("{} ", prefix)
        } else {
            String::new()
        };
        let prefix_len = prefix_str.chars().count();
        let first_line_width = width.saturating_sub(prefix_len).max(1);
        let cont_width = width.max(1);

        let wrapped = wrap_text(&msg.content, first_line_width, cont_width);
        let mut lines: Vec<Line> = Vec::new();

        for (i, text) in wrapped.into_iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if i == 0 && !prefix_str.is_empty() {
                spans.push(Span::styled(prefix_str.clone(), Style::default().fg(prefix_color)));
            }
            spans.push(Span::styled(text, Style::default().fg(text_color)));
            lines.push(Line::from(spans));
        }

        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                prefix_str,
                Style::default().fg(prefix_color),
            )]));
        }

        lines
    }

    fn render_input(&self, frame: &mut Frame, area: Rect, input: &InputState) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(self.theme.color_border_active()))
            .style(Style::default().bg(self.theme.color_background_panel()));

        let prompt_str = "> ";
        let cursor_pos = input.cursor();
        let buffer = input.buffer();
        let mut spans: Vec<Span> = vec![Span::styled(
            prompt_str,
            Style::default().fg(self.theme.color_primary()),
        )];
        if cursor_pos < buffer.len() {
            let before: String = buffer[..cursor_pos].chars().collect();
            let after: String = buffer[cursor_pos..].chars().collect();
            spans.push(Span::styled(
                before,
                Style::default().fg(self.theme.color_text()),
            ));
            spans.push(Span::styled(
                " ",
                Style::default()
                    .bg(self.theme.color_primary())
                    .fg(self.theme.color_primary()),
            ));
            spans.push(Span::styled(
                after,
                Style::default().fg(self.theme.color_text()),
            ));
        } else {
            spans.push(Span::styled(
                buffer,
                Style::default().fg(self.theme.color_text()),
            ));
            spans.push(Span::styled(
                " ",
                Style::default()
                    .bg(self.theme.color_primary())
                    .fg(self.theme.color_primary()),
            ));
        }

        let para = Paragraph::new(Line::from(spans)).block(block);
        frame.render_widget(para, area);
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect, status_text: &str) {
        let para = Paragraph::new(status_text)
            .style(
                Style::default()
                    .fg(self.theme.color_text_muted())
                    .bg(self.theme.color_background_panel()),
            );
        frame.render_widget(para, area);
    }
}
