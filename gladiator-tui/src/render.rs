use crate::state::{AppMessage, AppMessageRole, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Word-wrap text into lines at the given widths.
/// `first_width` is the available width for the first line (after prefix).
/// `cont_width` is the available width for continuation lines.
/// Returns a Vec of wrapped line strings (without prefix).
fn wrap_text(content: &str, first_width: usize, cont_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for word in content.split_whitespace() {
        let avail = if lines.is_empty() {
            first_width
        } else {
            cont_width
        };
        let word_len = word.chars().count();

        if current.is_empty() {
            if word_len > avail && avail > 0 {
                let mut remaining: Vec<char> = word.chars().collect();
                while !remaining.is_empty() {
                    let avail = if lines.is_empty() {
                        first_width
                    } else {
                        cont_width
                    };
                    let take: String = remaining.iter().take(avail).collect();
                    let take_len = take.chars().count();
                    lines.push(take);
                    remaining.drain(0..take_len.min(remaining.len()));
                }
            } else {
                current = word.to_string();
                current_len = word_len;
            }
        } else if current_len + 1 + word_len > avail {
            lines.push(std::mem::take(&mut current));
            if word_len > cont_width && cont_width > 0 {
                let mut remaining: Vec<char> = word.chars().collect();
                while !remaining.is_empty() {
                    let take: String = remaining.iter().take(cont_width).collect();
                    let take_len = take.chars().count();
                    lines.push(take);
                    remaining.drain(0..take_len.min(remaining.len()));
                }
            } else {
                current = word.to_string();
                current_len = word_len;
            }
        } else {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines
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
        let offset = if total_lines > visible_height {
            scroll.offset().min(total_lines.saturating_sub(visible_height))
        } else {
            0
        };

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
