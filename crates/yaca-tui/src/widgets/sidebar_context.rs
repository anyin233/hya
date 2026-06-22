use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::sidebar_files::push_files;
use super::sidebar_format::{
    connector_color, format_basis_points, format_number, saved_tokens, used_percent,
};
use super::sidebar_runtime::push_runtime;
use super::sidebar_stats::{TranscriptStats, transcript_stats};
use crate::AppState;
use crate::theme::Theme;

const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 200_000;

pub fn sidebar_lines(state: &AppState, theme: &Theme) -> Vec<Line<'static>> {
    let stats = transcript_stats(state);
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    push_title(&mut lines, state, theme);
    push_context_pilot(&mut lines, state, theme, &stats);
    push_context(&mut lines, state, theme, &stats);
    push_files(&mut lines, state, theme);
    push_mcp(&mut lines, state, theme);
    push_lsp(&mut lines, state, theme);
    push_agents(&mut lines, state, theme);
    push_runtime(&mut lines, state, theme);
    lines
}

fn push_title(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    let title = if state.session_label.trim().is_empty() {
        "context".to_string()
    } else {
        state.session_label.trim().to_string()
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "GUI",
            Style::default()
                .fg(theme.text)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {title}"),
            Style::default().fg(theme.muted).bg(theme.panel),
        ),
    ]));
    lines.push(Line::from(""));
}

fn push_context_pilot(
    lines: &mut Vec<Line<'static>>,
    state: &AppState,
    theme: &Theme,
    stats: &TranscriptStats,
) {
    push_section(lines, "ContextPilot", theme.text, theme);
    let session_saved = state
        .context
        .session_saved_tokens
        .map(saved_tokens)
        .unwrap_or_else(|| "~0 tokens".to_string());
    let all_time_saved = state
        .context
        .all_time_saved_tokens
        .map(saved_tokens)
        .unwrap_or_else(|| "n/a".to_string());
    let saved_percent = state
        .context
        .saved_percent_basis_points
        .map(format_basis_points)
        .unwrap_or_else(|| "0.0%".to_string());
    lines.push(meta(format!("session saved {session_saved}"), theme.text));
    lines.push(meta(format!("all-time saved {all_time_saved}"), theme.text));
    lines.push(meta(
        format!(
            "saved {saved_percent} of ~{} tokens",
            format_number(stats.estimated_tokens)
        ),
        theme.text,
    ));
}

fn push_context(
    lines: &mut Vec<Line<'static>>,
    state: &AppState,
    theme: &Theme,
    stats: &TranscriptStats,
) {
    lines.push(Line::from(""));
    push_section(lines, "Context", theme.text, theme);
    let current = state
        .context
        .current_tokens
        .unwrap_or(stats.estimated_tokens);
    let window = state
        .context
        .context_window_tokens
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
    lines.push(meta(
        format!("{} tokens", format_number(current)),
        theme.muted,
    ));
    lines.push(meta(
        format!("{}% used", used_percent(current, window)),
        theme.muted,
    ));
    lines.push(meta(
        format!("{} messages · {} tools", stats.messages, stats.tools),
        theme.muted,
    ));
    if stats.errors > 0 || stats.attachments > 0 {
        lines.push(meta(
            format!("{} errors · {} files", stats.errors, stats.attachments),
            theme.muted,
        ));
    }
    let spent = state
        .context
        .spent_label
        .as_deref()
        .or(state.cost_label.as_deref())
        .unwrap_or("$0.00");
    lines.push(meta(format!("{spent} spent"), theme.muted));
}

fn push_mcp(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    if state.mcp.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    let title = if state.mcp.len() > 2 {
        "▾ MCP"
    } else {
        "MCP"
    };
    push_section(lines, title, theme.text, theme);
    for connector in &state.mcp {
        lines.push(connector_line(
            &connector.name,
            connector.state.label(),
            connector_color(&connector.state, theme),
            theme,
        ));
    }
}

fn push_lsp(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    lines.push(Line::from(""));
    push_section(lines, "LSP", theme.text, theme);
    lines.push(meta(
        state
            .lsp_status
            .clone()
            .unwrap_or_else(|| "LSPs are disabled".to_string()),
        theme.muted,
    ));
}

fn push_agents(lines: &mut Vec<Line<'static>>, state: &AppState, theme: &Theme) {
    lines.push(Line::from(""));
    push_section(lines, "Agents", theme.info, theme);
    if state.team.is_empty() {
        lines.push(meta(format!("{} - active", agent_label(state)), theme.text));
        return;
    }
    for (member, status) in &state.team {
        let status = status.trim();
        let label = if status.is_empty() {
            member.to_string()
        } else {
            format!("{member} - {status}")
        };
        lines.push(meta(label, theme.text));
    }
}

pub(super) fn push_section(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    color: Color,
    theme: &Theme,
) {
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(color)
                .bg(theme.panel)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
}

pub(super) fn meta(text: impl Into<String>, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(text.into(), Style::default().fg(color)),
    ])
}

fn connector_line(name: &str, status: &str, marker: Color, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("  • ", Style::default().fg(marker)),
        Span::styled(name.to_string(), Style::default().fg(theme.text)),
        Span::raw(" "),
        Span::styled(status.to_string(), Style::default().fg(theme.muted)),
    ])
}

fn agent_label(state: &AppState) -> String {
    if state.agent.is_empty() {
        "build".to_string()
    } else {
        state.agent.clone()
    }
}
