//! `yaca-tui` — ratatui rendering of the projected agent state.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use yaca_proto::{Envelope, PartProjection, Projection};

#[derive(Default)]
pub struct AppState {
    pub projection: Projection,
    pub goal: Option<GoalView>,
    pub loop_view: Option<LoopView>,
    pub team: Vec<(String, String)>,
    pub pending_permission: Option<String>,
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
}

fn message_lines(projection: &Projection) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for m in &projection.session.messages {
        let mut text = String::new();
        for p in &m.parts {
            match p {
                PartProjection::Text { text: t, .. } => text.push_str(t),
                PartProjection::Tool { name, .. } => text.push_str(&format!("[tool:{name}]")),
                PartProjection::Reasoning { .. } => {}
            }
        }
        lines.push(Line::from(format!("{:?}: {text}", m.role)));
    }
    lines
}

pub fn draw(frame: &mut Frame, state: &AppState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let mut bars = Vec::new();
    if let Some(g) = &state.goal {
        bars.push(Line::from(format!(
            "GOAL: {} | turns {} | {}",
            g.condition, g.turns, g.last_reason
        )));
    }
    if let Some(l) = &state.loop_view {
        bars.push(Line::from(format!(
            "LOOP: {} | iter {}/{} | score {}",
            l.target, l.iteration, l.budget, l.last_score
        )));
    }
    if bars.is_empty() {
        bars.push(Line::from("yaca"));
    }
    frame.render_widget(
        Paragraph::new(bars).block(Block::default().title("status").borders(Borders::ALL)),
        rows[0],
    );

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(rows[1]);

    frame.render_widget(
        Paragraph::new(message_lines(&state.projection))
            .block(Block::default().title("messages").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        mid[0],
    );

    let team_lines: Vec<Line> = state
        .team
        .iter()
        .map(|(member, status)| Line::from(format!("{member}: {status}")))
        .collect();
    frame.render_widget(
        Paragraph::new(team_lines).block(Block::default().title("team").borders(Borders::ALL)),
        mid[1],
    );

    let footer = match &state.pending_permission {
        Some(req) => format!("PERMISSION REQUEST: {req}  [a]llow / [d]eny"),
        None => "[q]uit  [enter]send".to_string(),
    };
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        rows[2],
    );
}
