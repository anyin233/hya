//! `yaca-tui` — ratatui rendering of the projected agent state.
//!
//! Pure view: [`AppState`] holds the projection plus interaction state (input
//! buffer, scrollback, in-flight flag) and [`draw`] paints a chat layout. All
//! terminal I/O and the event loop live in the binary so this stays testable.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use yaca_proto::{Envelope, PartProjection, Projection, Role, ToolName, ToolPartState};

#[derive(Default)]
pub struct AppState {
    pub projection: Projection,
    pub goal: Option<GoalView>,
    pub loop_view: Option<LoopView>,
    pub team: Vec<(String, String)>,
    pub pending_permission: Option<String>,
    pub input: String,
    pub running: bool,
    pub scroll_back: u16,
    pub model: String,
    pub session_label: String,
}

pub struct GoalView {
    pub condition: String,
    pub turns: u32,
    pub last_reason: String,
}

pub struct LoopView {
    pub target: String,
    pub iteration: u32,
    pub budget: u32,
    pub last_score: u8,
}

impl AppState {
    pub fn apply(&mut self, envelope: &Envelope) {
        self.projection.apply(envelope);
    }

    pub fn scroll_up(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: u16) {
        self.scroll_back = self.scroll_back.saturating_sub(lines);
    }
}

fn role_style(role: Role) -> (&'static str, Color) {
    match role {
        Role::User => ("You", Color::Cyan),
        Role::Assistant => ("yaca", Color::Green),
        Role::System => ("sys", Color::DarkGray),
    }
}

fn ellipsize(s: &str, max: usize) -> String {
    let cleaned = s.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}

fn tool_input(state: &ToolPartState) -> String {
    let value = match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
    };
    if value.is_null() {
        String::new()
    } else {
        ellipsize(&value.to_string(), 48)
    }
}

fn tool_line(name: &ToolName, state: &ToolPartState) -> Line<'static> {
    let (status, color) = match state {
        ToolPartState::Completed { time_ms, .. } => (format!("✓ {time_ms}ms"), Color::Green),
        ToolPartState::Error { message, .. } => {
            (format!("✗ {}", ellipsize(message, 40)), Color::Red)
        }
        ToolPartState::Running { .. } | ToolPartState::Pending { .. } => {
            ("…".to_string(), Color::Yellow)
        }
    };
    Line::from(vec![
        Span::styled(format!("  ⚙ {name} "), Style::default().fg(Color::Magenta)),
        Span::raw(format!("{} ", tool_input(state))),
        Span::styled(status, Style::default().fg(color)),
    ])
}

fn transcript_lines(projection: &Projection) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for m in &projection.session.messages {
        let (label, color) = role_style(m.role);
        let header = Style::default().fg(color).add_modifier(Modifier::BOLD);
        let mut labelled = false;
        for part in &m.parts {
            match part {
                PartProjection::Text { text, .. } => {
                    for segment in text.split('\n') {
                        if labelled {
                            lines.push(Line::from(format!("     {segment}")));
                        } else {
                            lines.push(Line::from(vec![
                                Span::styled(format!("{label} "), header),
                                Span::raw(segment.to_string()),
                            ]));
                            labelled = true;
                        }
                    }
                }
                PartProjection::Tool { name, state, .. } => {
                    if !labelled {
                        lines.push(Line::from(Span::styled(format!("{label} "), header)));
                        labelled = true;
                    }
                    lines.push(tool_line(name, state));
                }
                PartProjection::Reasoning { .. } => {}
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Ask yaca anything. Type below and press Enter.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn status_line(state: &AppState) -> Line<'static> {
    let mut spans = vec![
        Span::styled("yaca", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" · {} · {}", state.model, state.session_label)),
        Span::styled(
            if state.running {
                "  ● streaming".to_string()
            } else {
                "  ○ idle".to_string()
            },
            Style::default().fg(if state.running {
                Color::Yellow
            } else {
                Color::DarkGray
            }),
        ),
    ];
    if let Some(g) = &state.goal {
        spans.push(Span::raw(format!(
            "  GOAL:{} turns {}",
            g.condition, g.turns
        )));
    }
    if let Some(l) = &state.loop_view {
        spans.push(Span::raw(format!(
            "  LOOP:{} iter {}/{} score {}",
            l.target, l.iteration, l.budget, l.last_score
        )));
    }
    Line::from(spans)
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    frame.render_widget(Paragraph::new(status_line(state)), rows[0]);

    let lines = transcript_lines(&state.projection);
    let inner_height = rows[1].height.saturating_sub(2);
    let inner_width = rows[1].width.saturating_sub(2).max(1);
    let total = lines.iter().fold(0u16, |acc, line| {
        let wrapped = u16::try_from(line.width())
            .unwrap_or(u16::MAX)
            .div_ceil(inner_width)
            .max(1);
        acc.saturating_add(wrapped)
    });
    let max_back = total.saturating_sub(inner_height);
    state.scroll_back = state.scroll_back.min(max_back);
    let top = max_back.saturating_sub(state.scroll_back);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("conversation").borders(Borders::ALL))
            .wrap(Wrap { trim: false })
            .scroll((top, 0)),
        rows[1],
    );

    if !state.team.is_empty() {
        let team: Vec<Line> = state
            .team
            .iter()
            .map(|(member, status)| Line::from(format!("{member}: {status}")))
            .collect();
        let overlay = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(rows[1]);
        frame.render_widget(
            Paragraph::new(team).block(Block::default().title("team").borders(Borders::ALL)),
            overlay[1],
        );
    }

    let input_row = rows[2];
    let input_widget = match &state.pending_permission {
        Some(req) => Paragraph::new(format!("PERMISSION REQUEST: {req}  [a]llow  [d]eny")).block(
            Block::default()
                .title("permission")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        ),
        None => Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::raw(state.input.clone()),
        ]))
        .block(
            Block::default()
                .title("message — Enter: send · Ctrl-C: quit · PgUp/PgDn: scroll")
                .borders(Borders::ALL),
        ),
    };
    frame.render_widget(input_widget, input_row);

    if state.pending_permission.is_none() && !state.running {
        let typed = u16::try_from(state.input.chars().count()).unwrap_or(u16::MAX);
        let rightmost = input_row.x + input_row.width.saturating_sub(2);
        let cursor_x = (input_row.x + 3).saturating_add(typed).min(rightmost);
        frame.set_cursor_position((cursor_x, input_row.y + 1));
    }
}
