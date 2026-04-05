//! ratatui layout and widget rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::state::{InputMode, OutputLine, ToolStatus, TuiState};

/// Draw the full TUI layout into the given frame.
pub fn draw(frame: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // output area
            Constraint::Length(1), // status bar
            Constraint::Length(3), // input box
        ])
        .split(frame.area());

    draw_output(frame, chunks[0], state);
    draw_status_bar(frame, chunks[1], state);
    draw_input(frame, chunks[2], state);

    if state.mode == InputMode::Approval {
        draw_approval_popup(frame, state);
    }
}

// ---------------------------------------------------------------------------
// Output area
// ---------------------------------------------------------------------------

fn draw_output(frame: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    for entry in &state.output {
        match entry {
            OutputLine::UserPrompt(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "> ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
            }
            OutputLine::AssistantText(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line)));
                }
                // If text ends without a newline, the last partial line is still shown.
            }
            OutputLine::SystemMessage(text) => {
                lines.push(Line::from(Span::styled(
                    text.as_str(),
                    Style::default().fg(Color::Yellow),
                )));
            }
            OutputLine::ToolActivity {
                tool_name,
                command,
                status,
            } => {
                let (icon, style) = match status {
                    ToolStatus::Running => ("⚙ ", Style::default().fg(Color::Blue)),
                    ToolStatus::Completed { .. } => ("✓ ", Style::default().fg(Color::Green)),
                    ToolStatus::Failed { .. } => ("✗ ", Style::default().fg(Color::Red)),
                };

                let label = match (command, status) {
                    (Some(cmd), ToolStatus::Running) => {
                        format!("Running: {cmd}")
                    }
                    (None, ToolStatus::Running) => {
                        format!("Running: {tool_name}")
                    }
                    (
                        _,
                        ToolStatus::Completed {
                            exit_code: Some(code),
                        },
                    ) => {
                        format!("Completed (exit {code})")
                    }
                    (_, ToolStatus::Completed { exit_code: None }) => "Completed".to_string(),
                    (_, ToolStatus::Failed { status: s }) => {
                        format!("Failed: {s}")
                    }
                };

                lines.push(Line::from(vec![
                    Span::styled(icon, style),
                    Span::styled(label, style),
                ]));
            }
        }
    }

    let total_lines = lines.len() as u16;
    let visible_height = area.height.saturating_sub(2); // account for block borders
    let scroll = if state.auto_scroll {
        total_lines.saturating_sub(visible_height)
    } else {
        total_lines
            .saturating_sub(visible_height)
            .saturating_sub(state.scroll_offset as u16)
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(frame: &mut Frame, area: Rect, state: &TuiState) {
    let bar = &state.status_bar;
    let busy = if state.is_processing {
        " [working]"
    } else {
        ""
    };
    let text = format!(
        " {} │ {} │ {}tok{}",
        bar.session_name, bar.model, bar.total_tokens, busy
    );
    let paragraph =
        Paragraph::new(text).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Input box
// ---------------------------------------------------------------------------

fn draw_input(frame: &mut Frame, area: Rect, state: &TuiState) {
    let style = if state.is_processing {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };

    let paragraph = Paragraph::new(format!("> {}", state.input))
        .style(style)
        .block(Block::default().borders(Borders::ALL).title("Input"));

    frame.render_widget(paragraph, area);

    // Position the cursor after "> " prefix + cursor_pos
    if !state.is_processing && state.mode == InputMode::Normal {
        let cursor_x = area.x + 2 + state.cursor_pos as u16; // "> " is 2 chars
        let cursor_y = area.y + 1; // inside the border
        frame.set_cursor_position((cursor_x.min(area.right().saturating_sub(2)), cursor_y));
    }
}

// ---------------------------------------------------------------------------
// Approval popup
// ---------------------------------------------------------------------------

fn draw_approval_popup(frame: &mut Frame, state: &TuiState) {
    let Some(approval) = &state.pending_approval else {
        return;
    };

    let severity_icon = match approval.severity {
        crate::gate::Severity::Low => "⚠️  ",
        crate::gate::Severity::Medium => "🟡 ",
        crate::gate::Severity::High => "🔴 ",
    };

    let text = Text::from(vec![
        Line::from(vec![
            Span::raw(severity_icon),
            Span::styled(
                approval.reason.as_str(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(format!("Command: {}", approval.command)),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" approve  "),
            Span::styled(
                "[n]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" deny"),
        ]),
    ]);

    let popup_width = 50u16.min(frame.area().width.saturating_sub(4));
    let popup_height = 6u16.min(frame.area().height.saturating_sub(4));
    let area = centered_rect(popup_width, popup_height, frame.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Approval Required")
        .style(Style::default().bg(Color::Black));

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}
