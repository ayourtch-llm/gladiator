use crate::state::{AppMessage, AppMessageRole, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

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
        let total = messages.len();

        let offset = if total > visible_height {
            scroll.offset().min(total.saturating_sub(visible_height))
        } else {
            0
        };

        let lines: Vec<Line> = messages
            .iter()
            .skip(offset)
            .take(visible_height)
            .map(|msg| self.message_to_line(msg))
            .collect();

        let para = Paragraph::new(lines).block(block);
        frame.render_widget(para, area);
    }

    fn message_to_line<'a>(&self, msg: &'a AppMessage) -> Line<'a> {
        let (prefix, prefix_color, text_color) = match msg.role {
            AppMessageRole::User => (
                ">",
                self.theme.color_secondary(),
                self.theme.color_text(),
            ),
            AppMessageRole::Assistant => (
                "",
                self.theme.color_primary(),
                self.theme.color_text(),
            ),
            AppMessageRole::Tool => (
                "[tool]",
                self.theme.color_info(),
                self.theme.color_info(),
            ),
            AppMessageRole::Error => (
                "[!]",
                self.theme.color_error(),
                self.theme.color_error(),
            ),
            AppMessageRole::Info => (
                "[i]",
                self.theme.color_info(),
                self.theme.color_text_muted(),
            ),
            AppMessageRole::System => (
                "[sys]",
                self.theme.color_text_muted(),
                self.theme.color_text_muted(),
            ),
        };

        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(
                format!("{} ", prefix),
                Style::default().fg(prefix_color),
            ));
        }
        spans.push(Span::styled(
            msg.content.as_str(),
            Style::default().fg(text_color),
        ));
        Line::from(spans)
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
