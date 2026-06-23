use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::AppState;
use crate::theme::Theme;

const CARD_WIDTH: usize = 34;
const INNER_WIDTH: usize = CARD_WIDTH - 2;
const CONTENT_WIDTH: usize = INNER_WIDTH - 1;

pub(super) fn push_agents(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    lines.push(Line::from(""));
    lines.push(border_line('┌', '┐', theme));
    lines.push(card_line("Agents", theme, true));
    if state.team.is_empty() {
        lines.push(agent_card_line(&agent_label(state), "active", theme));
    } else {
        for (member, status) in &state.team {
            lines.push(agent_card_line(member, status, theme));
        }
    }
    lines.push(border_line('└', '┘', theme));
}

fn border_line(left: char, right: char, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{left}{}{right}", "─".repeat(INNER_WIDTH)),
            Style::default().fg(theme.muted).bg(theme.panel),
        ),
    ])
}

fn card_line(text: impl Into<String>, theme: &Theme, title: bool) -> Line<'static> {
    let text = fit_cell(&text.into(), CONTENT_WIDTH);
    let padding = " ".repeat(INNER_WIDTH.saturating_sub(1 + UnicodeWidthStr::width(text.as_str())));
    let text_style = if title {
        Style::default()
            .fg(theme.info)
            .bg(theme.panel)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text).bg(theme.panel)
    };
    let border_style = Style::default().fg(theme.muted).bg(theme.panel);
    Line::from(vec![
        Span::raw("  "),
        Span::styled("│ ", border_style),
        Span::styled(text, text_style),
        Span::styled(padding, border_style),
        Span::styled("│", border_style),
    ])
}

fn agent_card_line(member: &str, status: &str, theme: &Theme) -> Line<'static> {
    let status = status.trim();
    let label = if status.is_empty() {
        member.to_string()
    } else {
        format!("{member} - {status}")
    };
    let visible = fit_cell(&label, CONTENT_WIDTH);
    let padding =
        " ".repeat(INNER_WIDTH.saturating_sub(1 + UnicodeWidthStr::width(visible.as_str())));
    let border_style = Style::default().fg(theme.muted).bg(theme.panel);
    let identity_style = Style::default().fg(theme.agent).bg(theme.panel);
    let status_style = Style::default().fg(theme.muted).bg(theme.panel);
    let mut spans = vec![Span::raw("  "), Span::styled("│ ", border_style)];
    if UnicodeWidthStr::width(member) <= CONTENT_WIDTH
        && let Some(rest) = visible.strip_prefix(member)
    {
        spans.push(Span::styled(member.to_string(), identity_style));
        spans.push(Span::styled(rest.to_string(), status_style));
    } else {
        spans.push(Span::styled(visible, identity_style));
    }
    spans.push(Span::styled(padding, border_style));
    spans.push(Span::styled("│", border_style));
    Line::from(spans)
}

fn fit_cell(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width - 1 {
            break;
        }
        width += ch_width;
        out.push(ch);
    }
    out.push('…');
    out
}

fn agent_label(state: &AppState) -> String {
    if state.agent.is_empty() {
        "build".to_string()
    } else {
        state.agent.clone()
    }
}
