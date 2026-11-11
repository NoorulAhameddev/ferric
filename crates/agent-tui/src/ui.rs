use super::app::{AppState, AppStatus, SPINNER_FRAMES};
use agent_core::Role;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

pub fn render(frame: &mut Frame, app: &AppState) {
    let line_count = app.input_buffer.split('\n').count() as u16;
    let input_height = (line_count + 2).clamp(3, 8);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),            // Header
            Constraint::Min(5),               // Messages viewport
            Constraint::Length(input_height), // Dynamic input area
        ])
        .split(frame.area());

    render_header(frame, app, chunks[0]);
    render_messages(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);

    if let AppStatus::PermissionPrompt { tool_name, target } = &app.status {
        render_permission_modal(frame, tool_name, target);
    }
}

fn render_header(frame: &mut Frame, app: &AppState, area: Rect) {
    let status_span = match &app.status {
        AppStatus::Idle => Span::styled(" ● Idle", Style::default().fg(Color::Green)),
        AppStatus::Running {
            step,
            spinner_frame,
        } => {
            let symbol = SPINNER_FRAMES[*spinner_frame];
            let activity_text = if let Some(ref act) = app.current_activity {
                format!(" {symbol} {act} (Step {step})")
            } else if !app.current_thinking.is_empty() {
                format!(" {symbol} Thinking... (Step {step})")
            } else {
                format!(" {symbol} Processing (Step {step})")
            };
            Span::styled(
                activity_text,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        }
        AppStatus::PermissionPrompt { .. } => {
            Span::styled(" ⚠ Permission Required", Style::default().fg(Color::Yellow))
        }
        AppStatus::Error(e) => Span::styled(
            format!(" ✖ Error: {e}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };

    let mut header_spans = vec![
        Span::styled(
            " Ferric Agent ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}] ", app.model),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("({})", app.provider),
            Style::default().fg(Color::Blue),
        ),
        Span::raw(" | Tokens: "),
        Span::styled(
            format!(
                "{}",
                app.total_usage.input_tokens + app.total_usage.output_tokens
            ),
            Style::default().fg(Color::Magenta),
        ),
    ];

    if app.plan_mode {
        header_spans.push(Span::styled(
            " | [PLAN]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    header_spans.push(Span::raw(" |"));
    header_spans.push(status_span);

    let title = Line::from(header_spans);

    let header_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(title)
        .block(header_block)
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, area);
}

fn render_messages(frame: &mut Frame, app: &AppState, area: Rect) {
    let mut lines = Vec::new();

    if app.messages.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "Welcome to Ferric Agent (Universal Rust Edition)!",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "Type a coding prompt below and press Enter. (Ctrl+C to cancel, Esc to exit)",
            Style::default().fg(Color::DarkGray),
        )]));
    }

    for msg in &app.messages {
        match msg.role {
            Role::User => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "You: ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(&msg.text, Style::default().fg(Color::White)),
                ]));
                lines.push(Line::raw(""));
            }
            Role::Assistant => {
                lines.push(Line::from(vec![Span::styled(
                    "Ferric: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));

                // Render completed thought process if captured
                if let Some(ref think) = msg.thinking {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            "💭 Thought Process",
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                        ),
                    ]));
                    for think_line in think.lines() {
                        lines.push(Line::from(vec![
                            Span::styled("    │ ", Style::default().fg(Color::DarkGray)),
                            Span::styled(
                                think_line,
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                    lines.push(Line::raw(""));
                }

                if !msg.text.is_empty() {
                    for text_line in msg.text.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(text_line, Style::default().fg(Color::White)),
                        ]));
                    }
                }

                // Render tool invocations
                for tc in &msg.tool_calls {
                    let icon = if tc.is_error { "✖" } else { "✔" };
                    let icon_color = if tc.is_error {
                        Color::Red
                    } else {
                        Color::Green
                    };

                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(icon, Style::default().fg(icon_color)),
                        Span::styled(
                            format!(" [{}] ", tc.name),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(&tc.input, Style::default().fg(Color::DarkGray)),
                    ]));

                    if let Some(ref out) = tc.output {
                        let preview: String = out.lines().take(3).collect::<Vec<_>>().join(" ");
                        let truncated = if out.lines().count() > 3 {
                            format!("{preview}...")
                        } else {
                            preview
                        };
                        lines.push(Line::from(vec![
                            Span::raw("    └─ "),
                            Span::styled(truncated, Style::default().fg(Color::Gray)),
                        ]));
                    }
                }
                lines.push(Line::raw(""));
            }
            Role::System => {}
        }
    }

    // Render live in-flight thinking or live activity while running
    if matches!(app.status, AppStatus::Running { .. }) {
        if !app.current_thinking.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "💭 Thinking...",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                ),
            ]));
            let recent_lines: Vec<&str> = app.current_thinking.lines().rev().take(6).collect();
            for think_line in recent_lines.into_iter().rev() {
                lines.push(Line::from(vec![
                    Span::styled("    │ ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        think_line,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            lines.push(Line::raw(""));
        }

        if let Some(ref act) = app.current_activity {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("⠋ {act}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::raw(""));
        }
    }

    let messages_block = Block::default()
        .borders(Borders::ALL)
        .title(" Conversation ")
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(lines)
        .block(messages_block)
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut Frame, app: &AppState, area: Rect) {
    let title = if let Some(ref comp) = app.completion {
        let total = comp.candidates.len();
        let curr = comp.current_index + 1;
        format!(" Prompt [Tab candidate {curr}/{total}] ")
    } else {
        " Prompt (Enter: submit, Shift+Enter: newline, Tab: complete) ".to_string()
    };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    let lines: Vec<Line> = app
        .input_buffer
        .split('\n')
        .map(|l| Line::from(Span::styled(l, Style::default().fg(Color::White))))
        .collect();

    let paragraph = Paragraph::new(lines).block(input_block);
    frame.render_widget(paragraph, area);

    // Position cursor in 2D
    let text_before_cursor = &app.input_buffer[..app.cursor_pos];
    let cursor_row = text_before_cursor.matches('\n').count() as u16;
    let line_start = text_before_cursor.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let cursor_col = (app.cursor_pos - line_start) as u16;

    let cursor_x = (area.x + 1 + cursor_col).min(area.right().saturating_sub(1));
    let cursor_y = (area.y + 1 + cursor_row).min(area.bottom().saturating_sub(1));
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn render_permission_modal(frame: &mut Frame, tool_name: &str, target: &str) {
    let area = centered_rect(60, 25, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Permission Request ")
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    let text = vec![
        Line::from(vec![Span::styled(
            "Tool Action Requires Approval:",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::raw(""),
        Line::from(vec![Span::styled(
            format!("Tool:   {tool_name}"),
            Style::default().fg(Color::Yellow),
        )]),
        Line::from(vec![Span::styled(
            format!("Action: {target}"),
            Style::default().fg(Color::White),
        )]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  [y] Allow Once  ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  [n] Deny  ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  [a] Always Allow  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
