use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::identity::active_agent_label;
use super::sidebar_format::used_percent;
use crate::AppState;
use crate::theme::Theme;

const COMPACT_METADATA_WIDTH: u16 = 80;
const COMMAND_HINT_WIDTH: u16 = 66;

pub(super) fn composer_metadata(state: &AppState, theme: &Theme, width: u16) -> Line<'static> {
    let policy = ComposerWidthPolicy::for_width(width);
    let agent = active_agent_label(state);
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let effort = state.reasoning_effort.as_deref().unwrap_or("off");
    let cost = state.cost_label.as_deref().unwrap_or("cost n/a");
    let mode = if state.yolo { "YOLO" } else { "manual" };
    let context = if policy.show_context_hints {
        composer_context_label(state).unwrap_or_default()
    } else {
        String::new()
    };
    let context_separator = if context.is_empty() { "" } else { " · " };
    let command_hint = if policy.show_command_hint {
        "   ctrl+p commands"
    } else {
        ""
    };
    let left_width = [
        "  ",
        agent.as_str(),
        " · ",
        model,
        " · think ",
        effort,
        " · ",
        mode,
    ]
    .into_iter()
    .map(UnicodeWidthStr::width)
    .sum::<usize>();
    let right_width = UnicodeWidthStr::width(context.as_str())
        + UnicodeWidthStr::width(context_separator)
        + if policy.show_context_hints {
            UnicodeWidthStr::width(cost)
        } else {
            0
        }
        + UnicodeWidthStr::width(command_hint);
    let status_gap_width = usize::from(width).saturating_sub(left_width + right_width);
    let mut spans = vec![
        Span::styled("  ", Style::default().bg(theme.element)),
        Span::styled(agent, Style::default().fg(theme.info).bg(theme.element)),
        Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
        Span::styled(
            model.to_string(),
            Style::default().fg(theme.text).bg(theme.element),
        ),
        Span::styled(
            " · think ",
            Style::default().fg(theme.muted).bg(theme.element),
        ),
        Span::styled(
            effort.to_string(),
            Style::default().fg(theme.accent).bg(theme.element),
        ),
        Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
        Span::styled(
            mode.to_string(),
            Style::default().fg(theme.warning).bg(theme.element),
        ),
        Span::styled(
            " ".repeat(status_gap_width),
            Style::default().bg(theme.element),
        ),
    ];
    if policy.show_context_hints {
        spans.extend([
            Span::styled(context, Style::default().fg(theme.muted).bg(theme.element)),
            Span::styled(
                context_separator,
                Style::default().fg(theme.muted).bg(theme.element),
            ),
            Span::styled(
                cost.to_string(),
                Style::default().fg(theme.muted).bg(theme.element),
            ),
        ]);
    }
    if policy.show_command_hint {
        spans.push(Span::styled(
            command_hint,
            Style::default().fg(theme.muted).bg(theme.element),
        ));
    }
    Line::from(spans)
}

struct ComposerWidthPolicy {
    show_context_hints: bool,
    show_command_hint: bool,
}

impl ComposerWidthPolicy {
    const fn for_width(width: u16) -> Self {
        Self {
            show_context_hints: width >= COMPACT_METADATA_WIDTH,
            show_command_hint: width >= COMMAND_HINT_WIDTH,
        }
    }
}

fn composer_context_label(state: &AppState) -> Option<String> {
    let current = state.context.current_tokens?;
    match state.context.context_window_tokens {
        Some(window) => Some(format!(
            "{} ({}%)",
            compact_tokens(current),
            used_percent(current, window)
        )),
        None => Some(compact_tokens(current)),
    }
}

fn compact_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = tokens.saturating_add(50_000) / 100_000;
        format!("{}.{:01}M", tenths / 10, tenths % 10)
    } else if tokens >= 1_000 {
        let tenths = tokens.saturating_add(50) / 100;
        format!("{}.{:01}K", tenths / 10, tenths % 10)
    } else {
        tokens.to_string()
    }
}
