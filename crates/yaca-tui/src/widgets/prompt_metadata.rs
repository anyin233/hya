use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::identity::active_agent_label;
use super::sidebar_format::used_percent;
use crate::AppState;
use crate::theme::Theme;

const COMPACT_METADATA_WIDTH: u16 = 80;
const COMMAND_HINT_WIDTH: u16 = 66;
const MODEL_METADATA_WIDTH: u16 = 120;

pub(super) fn composer_identity_metadata(
    state: &AppState,
    theme: &Theme,
    render_width: u16,
    policy_width: u16,
) -> Line<'static> {
    let policy = ComposerWidthPolicy::for_width(policy_width);
    let agent = active_agent_label(state);
    let model = if state.model.is_empty() {
        "offline"
    } else {
        state.model.as_str()
    };
    let effort = state.reasoning_effort.as_deref();
    let mode = state.yolo.then_some("YOLO");
    let model_width = if policy.show_model {
        UnicodeWidthStr::width(" · ") + UnicodeWidthStr::width(model)
    } else {
        0
    };
    let effort_width = effort.map_or(0, |label| {
        UnicodeWidthStr::width(" · ") + UnicodeWidthStr::width(label)
    });
    let mode_width = mode.map_or(0, |label| {
        UnicodeWidthStr::width(" · ") + UnicodeWidthStr::width(label)
    });
    let left_width = UnicodeWidthStr::width("  ")
        + UnicodeWidthStr::width(agent.as_str())
        + model_width
        + effort_width
        + mode_width;
    let status_gap_width = usize::from(render_width).saturating_sub(left_width);
    let mut spans = vec![
        Span::styled("  ", Style::default().bg(theme.element)),
        Span::styled(agent, Style::default().fg(theme.info).bg(theme.element)),
    ];
    if policy.show_model {
        spans.extend([
            Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
            Span::styled(
                model.to_string(),
                Style::default().fg(theme.text).bg(theme.element),
            ),
        ]);
    }
    if let Some(effort) = effort {
        spans.extend([
            Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
            Span::styled(
                effort.to_string(),
                Style::default().fg(theme.accent).bg(theme.element),
            ),
        ]);
    }
    if let Some(mode) = mode {
        spans.extend([
            Span::styled(" · ", Style::default().fg(theme.muted).bg(theme.element)),
            Span::styled(mode, Style::default().fg(theme.warning).bg(theme.element)),
        ]);
    }
    spans.push(Span::styled(
        " ".repeat(status_gap_width),
        Style::default().bg(theme.element),
    ));
    Line::from(spans)
}

pub(super) fn composer_footer_metadata(
    state: &AppState,
    theme: &Theme,
    render_width: u16,
    policy_width: u16,
    left_text: String,
) -> Line<'static> {
    let policy = ComposerWidthPolicy::for_width(policy_width);
    let context = if policy.show_context_hints {
        composer_context_label(state).unwrap_or_default()
    } else {
        String::new()
    };
    let cost = if policy.show_context_hints {
        state.cost_label.as_deref().unwrap_or("cost n/a")
    } else {
        ""
    };
    let context_separator = if context.is_empty() || cost.is_empty() {
        ""
    } else {
        " · "
    };
    let command_hint = if policy.show_command_hint {
        "ctrl+p commands"
    } else {
        ""
    };
    let command_separator = if command_hint.is_empty() || (context.is_empty() && cost.is_empty()) {
        ""
    } else {
        "   "
    };
    let left_width = UnicodeWidthStr::width(left_text.as_str());
    let right_width = UnicodeWidthStr::width(context.as_str())
        + UnicodeWidthStr::width(context_separator)
        + UnicodeWidthStr::width(cost)
        + UnicodeWidthStr::width(command_separator)
        + UnicodeWidthStr::width(command_hint);
    let status_gap_width = usize::from(render_width).saturating_sub(left_width + right_width);
    let footer_style = Style::default().fg(theme.muted).bg(theme.background);
    let mut spans = vec![Span::styled(left_text, footer_style)];
    spans.push(Span::styled(" ".repeat(status_gap_width), footer_style));
    if policy.show_context_hints {
        spans.extend([
            Span::styled(context, footer_style),
            Span::styled(context_separator, footer_style),
            Span::styled(cost.to_string(), footer_style),
        ]);
    }
    if policy.show_command_hint {
        spans.extend([
            Span::styled(command_separator, footer_style),
            Span::styled(command_hint, footer_style),
        ]);
    }
    Line::from(spans)
}

struct ComposerWidthPolicy {
    show_model: bool,
    show_context_hints: bool,
    show_command_hint: bool,
}

impl ComposerWidthPolicy {
    const fn for_width(width: u16) -> Self {
        Self {
            show_model: width >= MODEL_METADATA_WIDTH,
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
