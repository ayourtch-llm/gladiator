use crate::state::{AppMessage, AppMessageRole, ChatState, InputState, ScrollState};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Count the number of visual lines needed to display the input buffer,
/// accounting for hard-wrapping at the available width. The first logical
/// line is shorter by `prompt_len` (the "> " prefix).
fn count_input_visual_lines(buffer: &str, width: usize, prompt_len: usize) -> usize {
    let logical_lines: Vec<&str> = buffer.split('\n').collect();
    let mut count = 0usize;
    for (i, line) in logical_lines.iter().enumerate() {
        let avail = if i == 0 {
            width.saturating_sub(prompt_len).max(1)
        } else {
            width.max(1)
        };
        let chars_count = line.chars().count();
        if chars_count == 0 {
            count += 1;
        } else {
            count += (chars_count + avail - 1) / avail;
        }
    }
    count.max(1)
}

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
        pending_messages: &[String],
    ) {
        let area = frame.area();
        let term_width = area.width as usize;

        // Dynamic input height based on visual lines (hard-wrapped at width).
        // +1 for top border, capped at half the terminal height.
        let prompt_len = InputState::PROMPT_LEN; // "> "
        let input_visual_lines =
            count_input_visual_lines(input.buffer(), term_width, prompt_len);
        let input_height = (input_visual_lines + 1) as u16;
        let max_input_height = (area.height / 2).max(3);
        let input_height = input_height.min(max_input_height);

        // Pending messages panel: 1 row per message + 1 for title + 1 for top border.
        let pending_height = if pending_messages.is_empty() {
            0
        } else {
            (pending_messages.len() as u16 + 2).min((area.height / 4).max(3))
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(pending_height),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_messages(frame, chunks[1], chat, scroll);
        self.render_pending(frame, chunks[2], pending_messages);
        self.render_input(frame, chunks[3], input);
        self.render_status_bar(frame, chunks[4], status_text, scroll);
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

    fn render_pending(&self, frame: &mut Frame, area: Rect, pending_messages: &[String]) {
        if pending_messages.is_empty() || area.height == 0 {
            return;
        }

        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(self.theme.color_warning()))
            .style(Style::default().bg(self.theme.color_background()));

        let mut lines: Vec<Line> = Vec::new();
        let header = Line::from(vec![
            Span::styled(
                " pending messages ",
                Style::default()
                    .fg(self.theme.color_warning())
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        lines.push(header);

        // area.height includes 1 row consumed by top border + 1 row for the
        // "pending messages" header. Remaining rows show actual messages.
        let available = area.height as usize;
        let max_msgs = available.saturating_sub(2); // -1 border, -1 title
        for msg in pending_messages.iter().take(max_msgs) {
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(
                "  + ",
                Style::default().fg(self.theme.color_warning()),
            ));
            spans.push(Span::styled(
                msg.as_str(),
                Style::default().fg(self.theme.color_text()),
            ));
            lines.push(Line::from(spans));
        }

        let para = Paragraph::new(lines).block(block);
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
            AppMessageRole::Thinking => ("", self.theme.color_warning(), Color::Rgb(250, 220, 80)),
            AppMessageRole::Tool => ("[tool]", self.theme.color_info(), self.theme.color_info()),
            AppMessageRole::Error => ("[!]", self.theme.color_error(), self.theme.color_error()),
            AppMessageRole::Info => ("[i]", self.theme.color_info(), self.theme.color_text_muted()),
            AppMessageRole::System => ("[sys]", self.theme.color_text_muted(), self.theme.color_text_muted()),
        };

        // Subagent indentation: prepend "| " per depth level so nested output
        // is visually indented in the chat window.
        let indent_str = if msg.depth > 0 {
            "| ".repeat(msg.depth)
        } else {
            String::new()
        };
        let role_prefix = if !prefix.is_empty() {
            format!("{} ", prefix)
        } else {
            String::new()
        };
        // The full first-line prefix is indent + role prefix (e.g. "|| [tool]").
        let prefix_str = format!("{}{}", indent_str, role_prefix);
        let prefix_len = prefix_str.chars().count();

        // Tool messages: no wrapping, preserve exact whitespace, apply h_offset
        if msg.role == AppMessageRole::Tool {
            // Use structured tool_kind when available; fall back to content-
            // matching for legacy messages without metadata.
            let is_diff = match msg.tool_kind.as_ref() {
                Some(crate::state::ToolKind::Edit) => true,
                _ => msg.content.contains('\n')
                    && {
                        let first_line = msg.content.split('\n').next().unwrap_or("");
                        matches!(
                            first_line.trim(),
                            "edit_file" | "apply_edits" | "plan_edits"
                        )
                    },
            };

            if is_diff {
                let all_lines: Vec<&str> = msg.content.split('\n').collect();
                let mut lines: Vec<Line> = Vec::new();

                for (i, raw_line) in all_lines.iter().enumerate() {
                    // First line is the tool name — render with [tool] prefix.
                    if i == 0 && !prefix_str.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled(prefix_str.clone(), Style::default().fg(prefix_color)),
                            Span::styled(raw_line.to_string(), Style::default().fg(self.theme.color_info())),
                        ]));
                        continue;
                    }

                    let line_color = if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
                        self.theme.color_text_muted()
                    } else if raw_line.starts_with("@@") {
                        self.theme.color_accent()
                    } else if raw_line.starts_with('+') {
                        self.theme.color_success()
                    } else if raw_line.starts_with('-') {
                        self.theme.color_error()
                    } else if raw_line.starts_with('#') {
                        // Description comment from plan_edits
                        self.theme.color_text_muted()
                    } else {
                        text_color
                    };

                    let avail = if i == 0 {
                        width.saturating_sub(prefix_len).max(1)
                    } else {
                        width.saturating_sub(indent_str.chars().count()).max(1)
                    };
                    let chars: Vec<char> = raw_line.chars().collect();
                    let start = h_offset.min(chars.len());
                    let end = (start + avail).min(chars.len());
                    let text: String = chars[start..end].iter().collect();

                    let mut spans: Vec<Span> = Vec::new();
                    if i > 0 && !indent_str.is_empty() {
                        spans.push(Span::styled(indent_str.clone(), Style::default().fg(self.theme.color_text_muted())));
                    }
                    spans.push(Span::styled(
                        text,
                        Style::default().fg(line_color),
                    ));
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

            // Todo tool results: render all lines without collapsing, with
            // status-colored checkboxes. The todo list is meant to be seen
            // in full — truncating it defeats the purpose.
            let is_todo = match msg.tool_kind.as_ref() {
                Some(crate::state::ToolKind::Todo) => true,
                _ => false,
            };
            if is_todo {
                let all_lines: Vec<&str> = msg.content.split('\n').collect();
                let mut lines: Vec<Line> = Vec::new();

                for (i, raw_line) in all_lines.iter().enumerate() {
                    // Color the status glyph based on [x], [~], or [ ]
                    let line_color = if raw_line.contains("[x]") || raw_line.contains("✓") {
                        self.theme.color_success()
                    } else if raw_line.contains("[~]") || raw_line.contains("~") {
                        self.theme.color_warning()
                    } else if raw_line.starts_with("(todo list is empty)") {
                        self.theme.color_text_muted()
                    } else {
                        text_color
                    };

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
                    spans.push(Span::styled(text, Style::default().fg(line_color)));
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

            // Collapse long tool results: first N lines + ellipsis hint.
            // Command tools (bash/run_command) get a larger budget, others 3.
            let is_bash = match msg.tool_kind.as_ref() {
                Some(crate::state::ToolKind::Bash) => true,
                _ => msg.content.starts_with("$ "),
            };
            let max_lines = if is_bash { 10 } else { 3 };

            let all_lines: Vec<&str> = msg.content.split('\n').collect();
            let truncate = all_lines.len() > max_lines + 1;

            let mut lines: Vec<Line> = Vec::new();
            let render_lines: Vec<&str> = if truncate {
                all_lines[..max_lines].to_vec()
            } else {
                all_lines.clone()
            };

            for (i, raw_line) in render_lines.iter().enumerate() {
                let chars: Vec<char> = raw_line.chars().collect();
                let avail = if i == 0 {
                    width.saturating_sub(prefix_len).max(1)
                } else {
                    width.saturating_sub(indent_str.chars().count()).max(1)
                };
                let start = h_offset.min(chars.len());
                let end = (start + avail).min(chars.len());
                let text: String = chars[start..end].iter().collect();

                let mut spans: Vec<Span> = Vec::new();
                if i == 0 && !prefix_str.is_empty() {
                    spans.push(Span::styled(prefix_str.clone(), Style::default().fg(prefix_color)));
                } else if i > 0 && !indent_str.is_empty() {
                    spans.push(Span::styled(indent_str.clone(), Style::default().fg(self.theme.color_text_muted())));
                }
                spans.push(Span::styled(text, Style::default().fg(text_color)));
                lines.push(Line::from(spans));
            }

            // Append collapse hint when truncated
            if truncate {
                let hidden = all_lines.len() - max_lines;
                let hint = format!("… {} more lines ({} total) — scroll right/up not available; see full output in logs", hidden, all_lines.len());
                let mut spans: Vec<Span> = Vec::new();
                if !indent_str.is_empty() {
                    spans.push(Span::styled(indent_str.clone(), Style::default().fg(self.theme.color_text_muted())));
                }
                spans.push(Span::styled(
                    hint,
                    Style::default().fg(self.theme.color_text_muted()).add_modifier(Modifier::DIM),
                ));
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
        // When depth > 0, every continuation line also gets the indent prefix
        // so the entire message block is visually indented, not just line 1.
        let indent_for_cont = indent_str.clone();
        let first_line_width = width.saturating_sub(prefix_len).max(1);
        let cont_width = width.saturating_sub(indent_for_cont.chars().count()).max(1);

        let wrapped = wrap_text(&msg.content, first_line_width, cont_width);
        let mut lines: Vec<Line> = Vec::new();

        for (i, text) in wrapped.into_iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if i == 0 {
                if !prefix_str.is_empty() {
                    spans.push(Span::styled(prefix_str.clone(), Style::default().fg(prefix_color)));
                }
            } else if !indent_for_cont.is_empty() {
                spans.push(Span::styled(indent_for_cont.clone(), Style::default().fg(self.theme.color_text_muted())));
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
        let prompt_len: usize = InputState::PROMPT_LEN; // "> ".chars().count()
        let cursor_pos = input.cursor();
        let buffer = input.buffer();
        let width = area.width as usize;

        // Split buffer into logical lines by newline
        let logical_lines: Vec<&str> = buffer.split('\n').collect();

        // Find which logical line the cursor is on and byte offset within it
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
            byte_count += line_len + 1; // +1 for newline
            cursor_logical_line = i;
            cursor_byte_in_line = line_len;
        }
        // Edge case: cursor at end of buffer
        if cursor_pos == buffer.len() {
            cursor_logical_line = logical_lines.len() - 1;
            cursor_byte_in_line = logical_lines[cursor_logical_line].len();
        }

        // Convert byte offset to char offset within the logical line
        let cursor_line_str = logical_lines[cursor_logical_line];
        let cursor_char_in_line = cursor_line_str[..cursor_byte_in_line].chars().count();

        // Build visual lines with hard-wrapping, tracking cursor position
        let mut visual_lines: Vec<String> = Vec::new();
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
            let mut pos = 0usize;
            loop {
                let end = (pos + avail).min(chars.len());
                let chunk: String = chars[pos..end].iter().collect();

                // Check if cursor is on this visual line
                if !found_cursor && i == cursor_logical_line {
                    if cursor_char_in_line >= pos && cursor_char_in_line < end {
                        cursor_visual_line = visual_lines.len();
                        cursor_visual_col = cursor_char_in_line - pos;
                        found_cursor = true;
                    } else if cursor_char_in_line == end && end >= chars.len() {
                        // Cursor at end of logical line
                        cursor_visual_line = visual_lines.len();
                        cursor_visual_col = end - pos;
                        found_cursor = true;
                    }
                    // else: cursor at wrap boundary — will be found on next visual line
                }

                visual_lines.push(chunk);

                if end >= chars.len() {
                    break;
                }
                pos = end;
            }
        }

        // Fallback: cursor not placed (should not happen, but safety net)
        if !found_cursor {
            cursor_visual_line = visual_lines.len().saturating_sub(1);
            cursor_visual_col = visual_lines.last().map(|s| s.chars().count()).unwrap_or(0);
        }

        // Ensure at least one visual line
        if visual_lines.is_empty() {
            visual_lines.push(String::new());
            cursor_visual_line = 0;
            cursor_visual_col = 0;
        }

        // Render visual lines
        let mut line_widgets: Vec<Line> = Vec::new();
        for (vi, text) in visual_lines.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            if vi == 0 {
                spans.push(Span::styled(
                    prompt_str,
                    Style::default().fg(self.theme.color_primary()),
                ));
            }

            if vi == cursor_visual_line {
                let cursor_style = Style::default()
                    .bg(self.theme.color_primary())
                    .fg(self.theme.color_background());
                let chars: Vec<char> = text.chars().collect();
                let col = cursor_visual_col.min(chars.len());
                let before: String = chars[..col].iter().collect();
                let rest: String = chars[col..].iter().collect();
                spans.push(Span::styled(
                    before,
                    Style::default().fg(self.theme.color_text()),
                ));
                if !rest.is_empty() {
                    let first_char: String = rest.chars().take(1).collect();
                    let after: String = rest.chars().skip(1).collect();
                    spans.push(Span::styled(first_char, cursor_style));
                    spans.push(Span::styled(
                        after,
                        Style::default().fg(self.theme.color_text()),
                    ));
                } else {
                    // Cursor at end of line: empty block cursor
                    spans.push(Span::styled(" ", cursor_style));
                }
            } else {
                spans.push(Span::styled(
                    text.as_str(),
                    Style::default().fg(self.theme.color_text()),
                ));
            }

            line_widgets.push(Line::from(spans));
        }

        let para = Paragraph::new(line_widgets).block(block);
        frame.render_widget(para, area);

        // Position terminal cursor at the input cursor location.
        // Only visual line 0 has the "> " prefix.
        let cursor_x = if cursor_visual_line == 0 {
            area.left() + prompt_len as u16 + cursor_visual_col as u16
        } else {
            area.left() + cursor_visual_col as u16
        };
        let cursor_y = area.top() + 1 + cursor_visual_line as u16; // +1 for top border
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
