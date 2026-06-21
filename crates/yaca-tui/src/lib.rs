//! `yaca-tui` — ratatui rendering of the projected agent state.
//!
//! Pure view: [`AppState`] holds the projection plus interaction state (input
//! buffer, scrollback, in-flight flag) and [`draw`] paints a chat layout. All
//! terminal I/O and the event loop live in the binary so this stays testable.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use yaca_proto::{Envelope, PartProjection, Projection, Role, ToolName, ToolPartState};

pub mod input;
pub mod theme;

pub use input::InputState;
pub use theme::Theme;

#[derive(Default)]
pub struct AppState {
    pub projection: Projection,
    pub goal: Option<GoalView>,
    pub loop_view: Option<LoopView>,
    pub team: Vec<(String, String)>,
    pub permission: Option<PermissionPrompt>,
    pub question: Option<QuestionPrompt>,
    pub picker: Option<Picker>,
    pub input: InputState,
    pub running: bool,
    pub scroll_back: u16,
    pub model: String,
    pub session_label: String,
    pub yolo: bool,
    pub reasoning_effort: Option<String>,
    pub theme: Theme,
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

pub struct PermissionPrompt {
    pub title: String,
    pub detail: String,
    pub selected: usize,
}

impl PermissionPrompt {
    #[must_use]
    pub fn options(&self) -> [String; 3] {
        [
            "Allow once".to_string(),
            format!("Allow all {}", self.title),
            "Deny".to_string(),
        ]
    }
}

pub struct QuestionPrompt {
    pub prompt: String,
    pub options: Vec<String>,
    pub selected: usize,
    pub input: String,
    pub allow_custom: bool,
}

pub struct Picker {
    pub title: String,
    pub entries: Vec<String>,
    pub selected: usize,
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

fn role_style(role: Role, theme: &Theme) -> (&'static str, Color) {
    match role {
        Role::User => ("You", theme.secondary),
        Role::Assistant => ("yaca", theme.success),
        Role::System => ("sys", theme.text_muted),
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

fn tool_line(name: &ToolName, state: &ToolPartState, theme: &Theme) -> Line<'static> {
    let (status, color) = match state {
        ToolPartState::Completed { time_ms, .. } => (format!("✓ {time_ms}ms"), theme.success),
        ToolPartState::Error { message, .. } => {
            (format!("✗ {}", ellipsize(message, 40)), theme.error)
        }
        ToolPartState::Running { .. } | ToolPartState::Pending { .. } => {
            ("…".to_string(), theme.warning)
        }
    };
    Line::from(vec![
        Span::styled(format!("  ⚙ {name} "), Style::default().fg(theme.accent)),
        Span::raw(format!("{} ", tool_input(state))),
        Span::styled(status, Style::default().fg(color)),
    ])
}

fn transcript_lines(projection: &Projection, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for m in &projection.session.messages {
        let (label, color) = role_style(m.role, theme);
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
                    lines.push(tool_line(name, state, theme));
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
    let theme = &state.theme;
    let mut spans = vec![
        Span::styled("yaca", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" · {}", state.session_label)),
        Span::styled(
            if state.running {
                "  ● streaming".to_string()
            } else {
                "  ○ idle".to_string()
            },
            Style::default().fg(if state.running {
                theme.warning
            } else {
                theme.text_muted
            }),
        ),
    ];
    if state.yolo {
        spans.push(Span::styled(
            "  [YOLO]".to_string(),
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(effort) = &state.reasoning_effort {
        spans.push(Span::styled(
            format!("  think:{effort}"),
            Style::default().fg(theme.accent),
        ));
    }
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

fn footer_line() -> Line<'static> {
    Line::from("Enter send · Esc quit · ↑↓ history · PgUp/PgDn scroll")
}

fn model_label(model: &str) -> String {
    if model.len() > 16 {
        format!("{}…", &model[..16])
    } else {
        model.to_string()
    }
}

fn draw_input(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let theme = state.theme.clone();
    let model = model_label(&state.model);
    let prefix = format!("{model} › ");
    let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
    let usable = area.width.saturating_sub(2).max(1) as usize;
    let text_width = usable.saturating_sub(prefix_width);

    state.input.scroll_to_cursor(text_width);
    let (visible, _offset) = state.input.visible_slice(text_width);

    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme.text_muted)),
        Span::raw(visible.to_string()),
    ]);
    frame.render_widget(
        Paragraph::new(line)
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .style(Style::default().bg(theme.background_element)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );

    if !state.running
        && state.permission.is_none()
        && state.question.is_none()
        && state.picker.is_none()
    {
        let cursor_col = state.input.cursor_column();
        let cursor_x = area.x
            + 1
            + u16::try_from(prefix_width + cursor_col.saturating_sub(state.input.scroll_offset()))
                .unwrap_or(area.width);
        let cursor_x = cursor_x.min(area.x + area.width - 1);
        frame.set_cursor_position((cursor_x, area.y + 1));
    }
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let theme = state.theme.clone();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new(status_line(state)).block(
            Block::default()
                .borders(Borders::NONE)
                .style(Style::default().bg(theme.background_panel)),
        ),
        rows[0],
    );

    let transcript_area = rows[1];
    let padded = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(transcript_area);
    let inner = padded[1];
    let (conversation_area, team_area) = if state.team.is_empty() {
        (inner, None)
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(inner);
        (columns[0], Some(columns[1]))
    };

    let lines = transcript_lines(&state.projection, &theme);
    let inner_height = conversation_area.height;
    let inner_width = usize::from(conversation_area.width).max(1);
    let total = lines.iter().fold(0u16, |acc, line| {
        let wrapped = u16::try_from(line.width())
            .unwrap_or(u16::MAX)
            .div_ceil(inner_width as u16)
            .max(1);
        acc.saturating_add(wrapped)
    });
    let max_back = total.saturating_sub(inner_height);
    state.scroll_back = state.scroll_back.min(max_back);
    let top = max_back.saturating_sub(state.scroll_back);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::NONE)
                    .style(Style::default().bg(theme.background)),
            )
            .wrap(Wrap { trim: false })
            .scroll((top, 0)),
        conversation_area,
    );

    if let Some(team_area) = team_area {
        let team: Vec<Line> = state
            .team
            .iter()
            .map(|(member, status)| Line::from(format!("{member}: {status}")))
            .collect();
        frame.render_widget(
            Paragraph::new(team)
                .block(
                    Block::default()
                        .borders(Borders::NONE)
                        .style(Style::default().bg(theme.background_panel)),
                )
                .wrap(Wrap { trim: false }),
            team_area,
        );
    }

    draw_input(frame, state, rows[2]);

    frame.render_widget(
        Paragraph::new(footer_line()).block(
            Block::default()
                .borders(Borders::NONE)
                .style(Style::default().bg(theme.background_panel)),
        ),
        rows[3],
    );

    if let Some(prompt) = &state.permission {
        draw_permission(frame, prompt, &theme);
    } else if let Some(question) = &state.question {
        draw_question(frame, question, &theme);
    } else if let Some(picker) = &state.picker {
        draw_picker(frame, picker, &theme);
    }
}

fn overlay_rect(frame: &Frame, height: u16) -> Rect {
    let area = frame.area();
    let height = height.min(area.height);
    let width = area.width.saturating_sub(4).max(12);
    Rect {
        x: area.x + 2,
        y: area.height.saturating_sub(height),
        width,
        height,
    }
}

fn draw_permission(frame: &mut Frame, prompt: &PermissionPrompt, theme: &Theme) {
    let rect = overlay_rect(frame, 8);
    frame.render_widget(Clear, rect);
    let inner_width = usize::from(rect.width).saturating_sub(4);
    let option_spans: Vec<Span> = prompt
        .options()
        .iter()
        .enumerate()
        .flat_map(|(i, label)| {
            let style = if i == prompt.selected {
                Style::default()
                    .fg(theme.background)
                    .bg(theme.primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_muted)
            };
            vec![Span::styled(format!(" {label} "), style), Span::raw(" ")]
        })
        .collect();
    let lines = vec![
        Line::from(Span::styled(
            format!("{} wants to run:", prompt.title),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(ellipsize(&prompt.detail, inner_width)),
        Line::from(""),
        Line::from(option_spans),
        Line::from(Span::styled(
            "←/→ select · Enter confirm · Esc deny",
            Style::default().fg(theme.text_muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title("permission required")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.background_panel)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_question(frame: &mut Frame, q: &QuestionPrompt, theme: &Theme) {
    let extra = u16::try_from(q.options.len()).unwrap_or(0);
    let height = (7u16.saturating_add(extra)).min(frame.area().height).max(5);
    let rect = overlay_rect(frame, height);
    frame.render_widget(Clear, rect);
    let inner_width = usize::from(rect.width).saturating_sub(4);
    let mut lines = vec![
        Line::from(Span::styled(
            ellipsize(&q.prompt, inner_width),
            Style::default()
                .fg(theme.secondary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (i, opt) in q.options.iter().enumerate() {
        let style = if i == q.selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_muted)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} ", ellipsize(opt, inner_width)),
            style,
        )));
    }
    if q.options.is_empty() || q.allow_custom {
        lines.push(Line::from(format!("> {}", q.input)));
    }
    let hint = if q.options.is_empty() {
        "type your answer · Enter confirm · Esc cancel"
    } else if q.allow_custom {
        "↑/↓ select · type for custom · Enter confirm · Esc cancel"
    } else {
        "↑/↓ select · Enter confirm · Esc cancel"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(theme.text_muted),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title("question")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.background_panel)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn draw_picker(frame: &mut Frame, picker: &Picker, theme: &Theme) {
    let entry_rows = u16::try_from(picker.entries.len()).unwrap_or(u16::MAX);
    let height = entry_rows.saturating_add(4).min(frame.area().height).max(4);
    let rect = overlay_rect(frame, height);
    frame.render_widget(Clear, rect);
    let inner_width = usize::from(rect.width).saturating_sub(4);
    let mut lines = vec![Line::from(Span::styled(
        "↑/↓ select · Enter confirm · Esc cancel",
        Style::default().fg(theme.text_muted),
    ))];
    for (i, label) in picker.entries.iter().enumerate() {
        let style = if i == picker.selected {
            Style::default()
                .fg(theme.background)
                .bg(theme.primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_muted)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} ", ellipsize(label, inner_width)),
            style,
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(picker.title.clone())
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border))
                    .style(Style::default().bg(theme.background_panel)),
            )
            .wrap(Wrap { trim: false }),
        rect,
    );
}
