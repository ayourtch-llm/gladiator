use crate::state::{AppMessage, AppMessageRole, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Hard-wrap text into visual lines, preserving all whitespace including
/// leading spaces, tabs, and multiple spaces. Newlines in the content produce
/// line breaks. Each sub-line is hard-wrapped at the available width.
/// `first_width` is the available width for the first visual line (after prefix).
/// `cont_width` is the available width for all subsequent visual lines.
fn wrap_text(content: &str, first_width: usize, cont_width: usize) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    for line in content.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            result.push(String::new());
            continue;
        }

        // Hard-wrap this line at the available width
        let mut pos = 0usize;
        while pos < chars.len() {
            let width = if result.is_empty() && pos == 0 {
                // Very first visual line gets first_width (after prefix)
                first_width.max(1)
            } else {
                cont_width.max(1)
            };
            let end = (pos + width).min(chars.len());
            let chunk: String = chars[pos..end].iter().collect();
            result.push(chunk);
            pos = end;
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

        // Dynamic input height based on number of lines in buffer
        let input_lines = input.buffer().lines().count().max(1);
        let input_height = (input_lines + 1).min(8) as u16; // +1 for top border, max 8

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_messages(frame, chunks[1], chat, scroll);
        self.render_input(frame, chunks[2], input);
        self.render_status_bar(frame, chunks[3], status_text, scroll);
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
        let h_offset = scroll.h_offset();

        // Build a flat list of visual lines by wrapping each message
        let mut all_visual_lines: Vec<Line> = Vec::new();
        for msg in messages {
            let lines = self.message_to_visual_lines(msg, width, h_offset);
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

        // Scroll indicators: ↑ at top-right when scrolled up, ↓ at bottom-right when more below
        let indicator_color = Style::default().fg(self.theme.color_text_muted());
        if offset > 0 && total_lines > visible_height {
            let top_right = Rect::new(area.right().saturating_sub(1), area.top(), 1, 1);
            frame.render_widget(Paragraph::new(Line::from(vec![Span::styled(
                "\u{2191}",
                indicator_color,
            )])), top_right);
        }
        if offset + visible_height < total_lines && total_lines > visible_height {
            let bottom_right = Rect::new(
                area.right().saturating_sub(1),
                area.bottom().saturating_sub(1),
                1,
                1,
            );
            frame.render_widget(Paragraph::new(Line::from(vec![Span::styled(
                "\u{2193}",
                indicator_color,
            )])), bottom_right);
        }
    }

    fn message_to_visual_lines(&self, msg: &AppMessage, width: usize, h_offset: usize) -> Vec<Line<'_>> {
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

        // Tool messages: no wrapping, preserve exact whitespace, apply h_offset
        if msg.role == AppMessageRole::Tool {
            let mut lines: Vec<Line> = Vec::new();
            for (i, raw_line) in msg.content.split('\n').enumerate() {
                let chars: Vec<char> = raw_line.chars().collect();
                let avail = if i == 0 {
                    width.saturating_sub(prefix_len).max(1)
                } else {
                    width.max(1)
                };
                let start = h_offset.min(chars.len());
                let end = (start + avail).min(chars.len());
                let text: String = chars[start..end].iter().collect();

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
            return lines;
        }

        // Non-tool messages: hard-wrap preserving whitespace and newlines
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

        // Split buffer into lines by newline for multi-line rendering
        let lines: Vec<&str> = buffer.split('\n').collect();

        // Find which line the cursor is on and the offset within that line
        let mut char_count = 0usize;
        let mut cursor_line = 0;
        let mut cursor_col = 0usize; // byte offset within the line
        for (i, line) in lines.iter().enumerate() {
            let line_len = line.len();
            if cursor_pos <= char_count + line_len {
                cursor_line = i;
                cursor_col = cursor_pos - char_count;
                break;
            }
            char_count += line_len + 1; // +1 for the newline
            if i == lines.len() - 1 {
                cursor_line = i;
                cursor_col = line_len;
            }
        }

        // Handle edge case: cursor at end of last line
        if cursor_pos == buffer.len() {
            cursor_line = lines.len() - 1;
            cursor_col = lines[cursor_line].len();
        }

        let mut line_widgets: Vec<Line> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if i == 0 {
                spans.push(Span::styled(
                    prompt_str,
                    Style::default().fg(self.theme.color_primary()),
                ));
            }

            if i == cursor_line {
                // Render with cursor highlight
                let before: String = line[..cursor_col].chars().collect();
                let after: String = line[cursor_col..].chars().collect();
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
                    *line,
                    Style::default().fg(self.theme.color_text()),
                ));
            }

            line_widgets.push(Line::from(spans));
        }

        let para = Paragraph::new(line_widgets).block(block);
        frame.render_widget(para, area);

        // Position the terminal cursor at the input cursor location.
        // Only line 0 has the "> " prefix, so lines > 0 start at area.left().
        let prompt_len: u16 = 2; // "> "
        let cursor_line_str = lines[cursor_line];
        let cursor_col_chars = cursor_line_str[..cursor_col].chars().count() as u16;
        let cursor_x = if cursor_line == 0 {
            area.left() + prompt_len + cursor_col_chars
        } else {
            area.left() + cursor_col_chars
        };
        let cursor_y = area.top() + 1 + cursor_line as u16; // +1 for top border
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect, status_text: &str, scroll: &ScrollState) {
        let total = scroll.total_lines();
        let offset = scroll.offset();
        let h = scroll.h_offset();
        let visible = scroll.visible_height();

        let mut right_info = String::new();
        if total > visible && visible > 0 {
            let end = (offset + visible).min(total);
            right_info.push_str(&format!(" {}-{} of {}", offset + 1, end, total));
        }
        if h > 0 {
            right_info.push_str(&format!("  \u{2190}{}", h));
        }

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(right_info.chars().count() as u16),
            ])
            .split(area);

        let left = Paragraph::new(status_text)
            .style(
                Style::default()
                    .fg(self.theme.color_text_muted())
                    .bg(self.theme.color_background_panel()),
            );
        frame.render_widget(left, chunks[0]);

        if !right_info.is_empty() {
            let right = Paragraph::new(right_info)
                .style(
                    Style::default()
                        .fg(self.theme.color_info())
                        .bg(self.theme.color_background_panel()),
                );
            frame.render_widget(right, chunks[1]);
        }
    }
}
