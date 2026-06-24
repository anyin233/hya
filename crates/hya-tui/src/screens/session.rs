use hya_sdk::{MessageStore, Part, StoredPart};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::contracts::{PromptDoc, Rgba};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::render::{draw, markdown, scroll::ScrollState};
use crate::theme::{selected_foreground, ResolvedTheme};

use super::prompt_box::{self, PromptBoxView};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRender {
    pub text: Text,
    pub message_offsets: Vec<(String, usize)>,
}

#[must_use]
fn format_time(created_ms: i64) -> String {
    let secs = (created_ms / 1000).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

#[allow(clippy::too_many_arguments)]
pub fn timeline_text(
    store: &MessageStore,
    session_id: &str,
    pending: &[String],
    width: usize,
    agent_color: Rgba,
    agents: &[String],
    model_names: &[(String, String, String)],
    spinner: &str,
    show_timestamps: bool,
    theme: &ResolvedTheme,
) -> TimelineRender {
    let mut lines = Vec::new();
    let mut message_offsets = Vec::new();
    let messages = store
        .messages
        .get(session_id)
        .map_or(&[][..], Vec::as_slice);
    let revert_id = store
        .session(session_id)
        .and_then(|session| session.revert_message_id());
    let reverted_count = revert_id.map_or(0, |point| {
        messages
            .iter()
            .filter(|message| {
                message.role.as_deref() == Some("user") && message.id.as_str() >= point
            })
            .count()
    });
    let mut shown_user: Vec<String> = Vec::new();
    for message in messages {
        if let Some(point) = revert_id {
            if message.id.as_str() >= point {
                if message.role.as_deref() == Some("user") {
                    let parts = store.parts.get(&message.id).map_or(&[][..], Vec::as_slice);
                    let text = collect_text(parts);
                    if !text.trim().is_empty() {
                        shown_user.push(text);
                    }
                }
                if message.id.as_str() == point {
                    push_revert_banner(&mut lines, reverted_count, width, agent_color, theme);
                }
                continue;
            }
        }
        let parts = store.parts.get(&message.id).map_or(&[][..], Vec::as_slice);
        match message.role.as_deref() {
            Some("user") => {
                let text = collect_text(parts);
                if text.trim().is_empty() {
                    continue;
                }
                shown_user.push(text.clone());
                let timestamp = if show_timestamps {
                    message.time.created.map(format_time)
                } else {
                    None
                };
                message_offsets.push((message.id.clone(), lines.len()));
                push_user(
                    &mut lines,
                    &text,
                    width,
                    agent_color,
                    false,
                    timestamp.as_deref(),
                    theme,
                );
            }
            Some("assistant") => {
                let producer = message.rest.get("agent").and_then(serde_json::Value::as_str);
                let msg_color = prompt_box::agent_color(theme, agents, producer);
                push_assistant(&mut lines, parts, width, spinner, theme);
                push_assistant_footer(
                    &mut lines, message, messages, width, msg_color, model_names, theme,
                );
            }
            _ => {}
        }
    }
    let queued = store.is_working(session_id);
    for prompt in pending {
        if shown_user.iter().any(|shown| shown.trim() == prompt.trim()) {
            continue;
        }
        push_user(&mut lines, prompt, width, agent_color, queued, None, theme);
    }
    if lines.is_empty() {
        lines.push(Line(vec![Span::styled(
            "Waiting for session events...",
            Some(theme.text_muted),
            None,
            Attrs::default(),
        )]));
    }
    TimelineRender {
        text: Text(lines),
        message_offsets,
    }
}

pub(crate) enum LastAssistantMessageText {
    Text(String),
    NoAssistantMessage,
    NoTextParts,
    EmptyText,
}

#[must_use]
pub(crate) fn last_assistant_message_text(
    store: &MessageStore,
    session_id: &str,
) -> Option<String> {
    let message = last_assistant_message(store, session_id)?;
    let text_parts = last_assistant_text_parts(store, &message.id);
    if text_parts.is_empty() {
        return None;
    }
    let text = text_parts.join("\n");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_owned())
}

#[must_use]
pub(crate) fn last_assistant_message_text_status(
    store: &MessageStore,
    session_id: &str,
) -> LastAssistantMessageText {
    let Some(message) = last_assistant_message(store, session_id) else {
        return LastAssistantMessageText::NoAssistantMessage;
    };
    if let Some(text) = last_assistant_message_text(store, session_id) {
        return LastAssistantMessageText::Text(text);
    }
    if last_assistant_text_parts(store, &message.id).is_empty() {
        return LastAssistantMessageText::NoTextParts;
    }
    LastAssistantMessageText::EmptyText
}

fn last_assistant_message<'a>(
    store: &'a MessageStore,
    session_id: &str,
) -> Option<&'a hya_sdk::Message> {
    let revert_id = store
        .session(session_id)
        .and_then(|session| session.revert_message_id());
    store.messages.get(session_id).and_then(|messages| {
        messages.iter().rev().find(|message| {
            message.role.as_deref() == Some("assistant")
                && revert_id.is_none_or(|point| message.id.as_str() < point)
        })
    })
}

fn last_assistant_text_parts<'a>(store: &'a MessageStore, message_id: &str) -> Vec<&'a str> {
    store
        .parts
        .get(message_id)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter_map(|part| match &part.inner {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

pub(crate) fn timeline_dialog_items(
    store: &MessageStore,
    session_id: &str,
) -> Vec<(String, String, String)> {
    let Some(messages) = store.messages.get(session_id) else {
        return Vec::new();
    };
    let mut items = messages
        .iter()
        .filter_map(|message| {
            if message.role.as_deref() != Some("user") {
                return None;
            }
            let parts = store.parts.get(&message.id).map_or(&[][..], Vec::as_slice);
            let text = first_visible_text_part(parts)?;
            Some((
                message.id.clone(),
                text.replace('\n', " "),
                message.time.created.map_or_else(String::new, format_time),
            ))
        })
        .collect::<Vec<_>>();
    items.reverse();
    items
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Parent { count: usize },
    Child { index: usize, total: usize },
}

pub(crate) fn subagent_status(store: &MessageStore, session_id: &str) -> Option<SubagentStatus> {
    let session = store.session(session_id)?;
    if let Some(parent_id) = session.parent_id.as_deref() {
        let siblings = store.child_sessions(parent_id);
        let index = siblings
            .iter()
            .position(|sibling| sibling.id == session_id)
            .map_or(1, |position| position + 1);
        return Some(SubagentStatus::Child {
            index,
            total: siblings.len(),
        });
    }
    let count = store.child_sessions(session_id).len();
    (count > 0).then_some(SubagentStatus::Parent { count })
}

fn subagent_status_line(status: SubagentStatus, theme: &ResolvedTheme) -> Line {
    let (marker, marker_color, text) = match status {
        SubagentStatus::Parent { count } => (
            "● ",
            theme.success,
            format!(
                "{count} subagent{} · ctrl+x ↓ view",
                if count == 1 { "" } else { "s" }
            ),
        ),
        SubagentStatus::Child { index, total } if total > 1 => (
            "▲ ",
            theme.accent,
            format!("subagent {index}/{total} · ↑ parent · ←→ cycle"),
        ),
        SubagentStatus::Child { index, total } => (
            "▲ ",
            theme.accent,
            format!("subagent {index}/{total} · ↑ parent"),
        ),
    };
    Line(vec![
        Span::styled(marker, Some(marker_color), None, Attrs::default()),
        Span::styled(text, Some(theme.text_muted), None, Attrs::default()),
    ])
}

pub struct SessionView<'a> {
    pub store: &'a MessageStore,
    pub session_id: &'a str,
    pub pending: &'a [String],
    pub prompt: &'a PromptDoc,
    pub agents: &'a [String],
    pub model_names: &'a [(String, String, String)],
    pub active_agent: Option<&'a str>,
    pub model_label: Option<&'a str>,
    pub provider_label: Option<&'a str>,
    pub context_limit: Option<i64>,
    pub spinner: &'a str,
    pub show_timestamps: bool,
    pub sidebar_visible: bool,
    pub subagent: Option<SubagentStatus>,
    pub show_cursor: bool,
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    view: &SessionView<'_>,
    scroll: &mut ScrollState,
    theme: &ResolvedTheme,
) -> prompt_box::PromptHits {
    let SessionView {
        store,
        session_id,
        pending,
        prompt,
        agents,
        model_names,
        active_agent,
        model_label,
        provider_label,
        context_limit,
        spinner,
        show_timestamps,
        sidebar_visible,
        subagent,
        show_cursor,
    } = *view;
    let area = frame.area();
    let background = theme.background;
    frame.render_widget(
        Block::default().style(
            ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
        ),
        area,
    );

    let (main_area, sidebar_area) = if area.width > 120 && sidebar_visible {
        let columns = ratatui::layout::Layout::horizontal([
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(SIDEBAR_WIDTH),
        ])
        .split(area);
        (columns[0], Some(columns[1]))
    } else {
        (area, None)
    };

    let prompt_rows = prompt_box::box_height(&prompt.text);
    let subagent_rows = u16::from(subagent.is_some());
    let chunks = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(1),
        ratatui::layout::Constraint::Length(subagent_rows),
        ratatui::layout::Constraint::Length(prompt_rows),
    ])
    .split(main_area);

    let timeline_agent_color = prompt_box::agent_color(theme, agents, active_agent);
    let timeline = timeline_text(
        store,
        session_id,
        pending,
        chunks[0].width as usize,
        timeline_agent_color,
        agents,
        model_names,
        spinner,
        show_timestamps,
        theme,
    );
    let old_height = scroll.content_height;
    scroll.viewport_height = chunks[0].height as usize;
    scroll.sticky_bottom(old_height, timeline.text.0.len());

    let body = draw::text_to_ratatui(&timeline.text, background);
    let messages = Paragraph::new(body)
        .scroll((scroll.offset as u16, 0))
        .wrap(Wrap { trim: false })
        .style(ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background)));
    frame.render_widget(messages, chunks[0]);

    let agent_label = active_agent.map(prompt_box::titlecase);
    let view = PromptBoxView {
        text: &prompt.text,
        placeholder: SESSION_PLACEHOLDER,
        agent_label: agent_label.as_deref(),
        agent_color: timeline_agent_color,
        model_label,
        provider_label,
        shell_mode: false,
        working: store.is_working(session_id),
        spinner,
        agent_shortcut: "tab",
        palette_shortcut: "ctrl+p",
        cursor: prompt.cursor,
        show_cursor,
    };
    if let Some(status) = subagent {
        let line = subagent_status_line(status, theme);
        let rendered = draw::text_to_ratatui(&Text(vec![line]), background);
        frame.render_widget(
            Paragraph::new(rendered).style(
                ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
            ),
            chunks[1],
        );
    }

    let prompt_area = crate::contracts::Rect {
        x: chunks[2].x,
        y: chunks[2].y,
        width: chunks[2].width,
        height: chunks[2].height,
    };

    let hits = prompt_box::draw(frame, prompt_area, &view, theme);

    if let Some(sidebar_area) = sidebar_area {
        draw_sidebar(frame, sidebar_area, store, session_id, context_limit, theme);
    }
    hits
}

const SESSION_PLACEHOLDER: &str = "Ask anything... \"Fix a TODO in the codebase\"";

pub(crate) const SIDEBAR_WIDTH: u16 = 42;

pub(crate) fn draw_sidebar(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    store: &MessageStore,
    session_id: &str,
    context_limit: Option<i64>,
    theme: &ResolvedTheme,
) {
    let background = theme.background;
    frame.render_widget(
        Block::default().style(
            ratatui::style::Style::default()
                .bg(draw::rgba_to_color(theme.background_panel, background)),
        ),
        area,
    );
    let inner = ratatui::layout::Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line> = Vec::new();
    if let Some(title) = store
        .session(session_id)
        .and_then(|session| session.title.as_deref())
    {
        let titled = Line(vec![Span::styled(title, Some(theme.text), None, bold())]);
        lines.extend(titled.wrap(inner.width as usize));
        lines.push(Line::default());
    }

    lines.push(Line(vec![Span::styled(
        "Context",
        Some(theme.text),
        None,
        bold(),
    )]));
    let tokens = context_tokens(store, session_id).unwrap_or(0);
    lines.push(muted_line(format!("{} tokens", thousands(tokens)), theme));
    if let Some(limit) = context_limit.filter(|limit| *limit > 0) {
        let percent = ((tokens as f64 / limit as f64) * 100.0).round() as i64;
        lines.push(muted_line(format!("{percent}% used"), theme));
    }
    let cost = session_cost(store, session_id).unwrap_or(0.0);
    lines.push(muted_line(format!("${cost:.2} spent"), theme));

    push_todo_block(&mut lines, store.todos(session_id), theme);
    push_files_block(
        &mut lines,
        store.diffs(session_id),
        inner.width as usize,
        theme,
    );

    let body = draw::text_to_ratatui(&Text(lines), background);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
}

fn push_todo_block(lines: &mut Vec<Line>, todos: &[serde_json::Value], theme: &ResolvedTheme) {
    let status = |todo: &serde_json::Value| {
        todo.get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if todos.is_empty() || todos.iter().all(|todo| status(todo) == "completed") {
        return;
    }
    lines.push(Line::default());
    lines.push(Line(vec![Span::styled(
        "Todo",
        Some(theme.text),
        None,
        bold(),
    )]));
    for todo in todos {
        let content = todo
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let state = status(todo);
        let (icon, color) = match state.as_str() {
            "completed" => ("\u{2713}", theme.text_muted),
            "in_progress" => ("\u{2022}", theme.warning),
            _ => (" ", theme.text_muted),
        };
        lines.push(Line(vec![Span::styled(
            format!("[{icon}] {content}"),
            Some(color),
            None,
            Attrs::default(),
        )]));
    }
}

fn push_files_block(
    lines: &mut Vec<Line>,
    diffs: &[serde_json::Value],
    width: usize,
    theme: &ResolvedTheme,
) {
    if diffs.is_empty() {
        return;
    }
    lines.push(Line::default());
    lines.push(Line(vec![Span::styled(
        "Modified Files",
        Some(theme.text),
        None,
        bold(),
    )]));
    let number = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    };
    for diff in diffs {
        let file = diff
            .get("file")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let additions = number(diff, "additions");
        let deletions = number(diff, "deletions");
        let counts = format!(
            "{}{}",
            if additions > 0 {
                format!(" +{additions}")
            } else {
                String::new()
            },
            if deletions > 0 {
                format!(" -{deletions}")
            } else {
                String::new()
            },
        );
        let budget = width.saturating_sub(counts.len()).max(2);
        let shown = truncate_left(file, budget);
        let mut spans = vec![Span::styled(
            shown,
            Some(theme.text_muted),
            None,
            Attrs::default(),
        )];
        if additions > 0 {
            spans.push(Span::styled(
                format!(" +{additions}"),
                Some(theme.diff_added),
                None,
                Attrs::default(),
            ));
        }
        if deletions > 0 {
            spans.push(Span::styled(
                format!(" -{deletions}"),
                Some(theme.diff_removed),
                None,
                Attrs::default(),
            ));
        }
        lines.push(Line(spans));
    }
}

fn truncate_left(text: &str, budget: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= budget {
        return text.to_owned();
    }
    let tail: String = chars[chars.len().saturating_sub(budget.saturating_sub(1))..]
        .iter()
        .collect();
    format!("…{tail}")
}

fn muted_line(text: String, theme: &ResolvedTheme) -> Line {
    Line(vec![Span::styled(
        text,
        Some(theme.text_muted),
        None,
        Attrs::default(),
    )])
}

fn context_tokens(store: &MessageStore, session_id: &str) -> Option<i64> {
    let messages = store.messages.get(session_id)?;
    let last = messages.iter().rev().find(|message| {
        message.role.as_deref() == Some("assistant") && token_value(message, &["output"]) > 0
    })?;
    Some(
        token_value(last, &["input"])
            + token_value(last, &["output"])
            + token_value(last, &["reasoning"])
            + token_value(last, &["cache", "read"])
            + token_value(last, &["cache", "write"]),
    )
}

fn token_value(message: &hya_sdk::Message, path: &[&str]) -> i64 {
    let mut value = match message.rest.get("tokens") {
        Some(value) => value,
        None => return 0,
    };
    for key in path {
        match value.get(key) {
            Some(next) => value = next,
            None => return 0,
        }
    }
    value.as_i64().unwrap_or(0)
}

fn session_cost(store: &MessageStore, session_id: &str) -> Option<f64> {
    store.session(session_id)?.rest.get("cost")?.as_f64()
}

fn thousands(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (bytes.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn collect_text(parts: &[StoredPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match &part.inner {
            Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn first_visible_text_part(parts: &[StoredPart]) -> Option<&str> {
    parts.iter().find_map(|part| match &part.inner {
        Part::Text { text, rest } => {
            let synthetic = rest
                .get("synthetic")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let ignored = rest
                .get("ignored")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            (!synthetic && !ignored).then_some(text.as_str())
        }
        _ => None,
    })
}

const USER_BAR: &str = "┃";
const ASSISTANT_INDENT: &str = "   ";

fn push_revert_banner(
    lines: &mut Vec<Line>,
    reverted: usize,
    width: usize,
    agent_color: Rgba,
    theme: &ResolvedTheme,
) {
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    let panel = theme.background_panel;
    let bar = || Span::styled(USER_BAR, Some(agent_color), None, Attrs::default());
    let content_width = width.saturating_sub(1);
    lines.push(Line(vec![bar(), panel_pad(content_width, panel)]));

    let label = if reverted == 1 {
        "1 message reverted".to_owned()
    } else {
        format!("{reverted} messages reverted")
    };
    let label_span = Span::styled(label, Some(theme.text_muted), Some(panel), Attrs::default());
    let used = 2 + label_span.width();
    let mut spans = vec![
        bar(),
        Span::styled("  ", Some(theme.text), Some(panel), Attrs::default()),
        label_span,
    ];
    if content_width > used {
        spans.push(panel_pad(content_width - used, panel));
    }
    lines.push(Line(spans));

    let key = Span::styled("ctrl+x r", Some(theme.text), Some(panel), Attrs::default());
    let rest = Span::styled(
        " or /redo to restore",
        Some(theme.text_muted),
        Some(panel),
        Attrs::default(),
    );
    let used = 2 + key.width() + rest.width();
    let mut spans = vec![
        bar(),
        Span::styled("  ", Some(theme.text), Some(panel), Attrs::default()),
        key,
        rest,
    ];
    if content_width > used {
        spans.push(panel_pad(content_width - used, panel));
    }
    lines.push(Line(spans));

    lines.push(Line(vec![bar(), panel_pad(content_width, panel)]));
}

fn push_user(
    lines: &mut Vec<Line>,
    text: &str,
    width: usize,
    agent_color: Rgba,
    queued: bool,
    timestamp: Option<&str>,
    theme: &ResolvedTheme,
) {
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    let panel = theme.background_panel;
    let bar = || Span::styled(USER_BAR, Some(agent_color), None, Attrs::default());
    let inner = width.saturating_sub(3).max(1);
    let content_width = width.saturating_sub(1);
    lines.push(Line(vec![bar(), panel_pad(content_width, panel)]));
    let wrapped = Line(vec![Span::styled(
        text,
        Some(theme.text),
        Some(panel),
        Attrs::default(),
    )])
    .wrap(inner);
    for line in wrapped {
        let used = line.width() + 2;
        let mut spans = vec![
            bar(),
            Span::styled("  ", Some(theme.text), Some(panel), Attrs::default()),
        ];
        spans.extend(line.0);
        if content_width > used {
            spans.push(panel_pad(content_width - used, panel));
        }
        lines.push(Line(spans));
    }
    if queued {
        let badge = Span::styled(
            " QUEUED ",
            Some(selected_foreground(theme, Some(agent_color))),
            Some(agent_color),
            Attrs {
                bold: true,
                ..Attrs::default()
            },
        );
        let used = 2 + badge.width();
        let mut spans = vec![
            bar(),
            Span::styled("  ", Some(theme.text), Some(panel), Attrs::default()),
            badge,
        ];
        if content_width > used {
            spans.push(panel_pad(content_width - used, panel));
        }
        lines.push(Line(spans));
    } else if let Some(timestamp) = timestamp {
        let stamp = Span::styled(
            timestamp,
            Some(theme.text_muted),
            Some(panel),
            Attrs::default(),
        );
        let used = 2 + stamp.width();
        let mut spans = vec![
            bar(),
            Span::styled("  ", Some(theme.text), Some(panel), Attrs::default()),
            stamp,
        ];
        if content_width > used {
            spans.push(panel_pad(content_width - used, panel));
        }
        lines.push(Line(spans));
    }
    lines.push(Line(vec![bar(), panel_pad(content_width, panel)]));
}

fn panel_pad(width: usize, panel: Rgba) -> Span {
    Span::styled(" ".repeat(width), None, Some(panel), Attrs::default())
}

fn push_assistant(
    lines: &mut Vec<Line>,
    parts: &[StoredPart],
    width: usize,
    spinner: &str,
    theme: &ResolvedTheme,
) {
    let inner = width.saturating_sub(ASSISTANT_INDENT.len()).max(1);
    for part in parts {
        match &part.inner {
            Part::Text { text, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                lines.push(Line::default());
                for line in markdown::parse(text, theme).wrap(inner).0 {
                    let mut spans = vec![Span::plain(ASSISTANT_INDENT)];
                    spans.extend(line.0);
                    lines.push(Line(spans));
                }
            }
            Part::Reasoning { text, rest } => {
                if text.trim().is_empty() {
                    continue;
                }
                let done = rest.get("time").and_then(|time| time.get("end")).is_some();
                push_reasoning(lines, text, done, spinner, theme);
            }
            Part::Tool(tool) => {
                lines.push(Line::default());
                for line in super::tool_part::render(tool, inner, spinner, theme) {
                    let mut spans = vec![Span::plain(ASSISTANT_INDENT)];
                    spans.extend(line.0);
                    lines.push(Line(spans));
                }
            }
            _ => {}
        }
    }
}

fn push_assistant_footer(
    lines: &mut Vec<Line>,
    message: &hya_sdk::Message,
    messages: &[hya_sdk::Message],
    _width: usize,
    agent_color: Rgba,
    model_names: &[(String, String, String)],
    theme: &ResolvedTheme,
) {
    if message.time.completed.is_none() {
        return;
    }
    let mode = message
        .rest
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .map(super::prompt_box::titlecase);
    let model = message_model_name(message, model_names);
    let variant = message
        .rest
        .get("variant")
        .and_then(serde_json::Value::as_str);
    let mut spans = vec![
        Span::plain(ASSISTANT_INDENT),
        Span::styled("\u{25a3}  ", Some(agent_color), None, Attrs::default()),
    ];
    if let Some(mode) = &mode {
        spans.push(Span::styled(
            mode.clone(),
            Some(theme.text),
            None,
            Attrs::default(),
        ));
    }
    if let Some(model) = &model {
        spans.push(Span::styled(
            format!(" \u{b7} {model}"),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ));
    }
    if let Some(variant) = variant {
        spans.push(Span::styled(
            format!(" [{variant}]"),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ));
    }
    if let Some(duration) = assistant_duration(message, messages) {
        spans.push(Span::styled(
            format!(" \u{b7} {duration}"),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ));
    }
    lines.push(Line::default());
    lines.push(Line(spans));
}

fn message_model_name(
    message: &hya_sdk::Message,
    model_names: &[(String, String, String)],
) -> Option<String> {
    let model_id = message
        .rest
        .get("modelID")
        .and_then(serde_json::Value::as_str)?;
    if let Some(provider_id) = message
        .rest
        .get("providerID")
        .and_then(serde_json::Value::as_str)
    {
        let value = format!("{provider_id}/{model_id}");
        if let Some((_, title, _)) = model_names
            .iter()
            .find(|(candidate, _, _)| *candidate == value)
        {
            return Some(title.clone());
        }
    }
    Some(model_id.to_owned())
}

fn assistant_duration(
    message: &hya_sdk::Message,
    messages: &[hya_sdk::Message],
) -> Option<String> {
    let completed = message.time.completed?;
    let parent_id = message
        .rest
        .get("parentID")
        .and_then(serde_json::Value::as_str)?;
    let started = messages
        .iter()
        .find(|candidate| candidate.id == parent_id)
        .and_then(|parent| parent.time.created)?;
    let seconds = (completed - started) as f64 / 1000.0;
    if seconds <= 0.0 {
        return None;
    }
    if seconds < 60.0 {
        return Some(format!("{seconds:.1}s"));
    }
    let minutes = (seconds / 60.0).floor();
    let remainder = (seconds - minutes * 60.0).round();
    Some(format!("{minutes:.0}m {remainder:.0}s"))
}

fn push_reasoning(
    lines: &mut Vec<Line>,
    text: &str,
    done: bool,
    spinner: &str,
    theme: &ResolvedTheme,
) {
    let content = text.replace("[REDACTED]", "");
    let content = content.trim();
    if content.is_empty() {
        return;
    }
    let label = if done { "Thought" } else { "Thinking" };
    let prefix = if done {
        ASSISTANT_INDENT.to_owned()
    } else {
        format!("{ASSISTANT_INDENT}{spinner} ")
    };
    let header = match reasoning_title(content) {
        Some(title) => format!("{prefix}{label}: {title}"),
        None => format!("{prefix}{label}"),
    };
    lines.push(Line::default());
    lines.push(Line(vec![Span::styled(
        header,
        Some(theme.warning),
        None,
        Attrs::default(),
    )]));
}

fn reasoning_title(content: &str) -> Option<String> {
    let trimmed = content.trim_start().strip_prefix("**")?;
    let end = trimmed.find("**")?;
    let title = trimmed[..end].trim();
    if title.is_empty() || title.contains('\n') {
        return None;
    }
    Some(title.to_owned())
}

const fn bold() -> Attrs {
    Attrs {
        bold: true,
        italic: false,
        underline: false,
        dim: false,
        strikethrough: false,
    }
}

#[must_use]
pub fn background(theme: &ResolvedTheme) -> Rgba {
    theme.background
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};
    use hya_sdk::GlobalEvent;

    fn theme() -> ResolvedTheme {
        resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap()
    }

    fn event(kind: &str, properties: serde_json::Value) -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .unwrap()
    }

    fn flatten(text: &Text) -> String {
        text.0
            .iter()
            .flat_map(|line| line.0.iter().map(|span| span.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn assistant_footer_uses_producing_agent_color_and_shows_model_and_variant() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_u", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_u", "messageID": "msg_u", "sessionID": "ses_1", "type": "text", "text": "do it" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": {
                "id": "msg_a", "sessionID": "ses_1", "role": "assistant", "parentID": "msg_u",
                "mode": "general", "agent": "general",
                "providerID": "prov", "modelID": "model-x", "variant": "high",
                "time": { "created": 2, "completed": 3000 }
            } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_a", "messageID": "msg_a", "sessionID": "ses_1", "type": "text", "text": "done" } }),
        ));

        let theme = theme();
        let agents = vec!["build".to_owned(), "general".to_owned()];
        let model_names = vec![("prov/model-x".to_owned(), "Model X".to_owned(), "Prov".to_owned())];
        let selected_color = prompt_box::agent_color(&theme, &agents, Some("build"));
        let render = timeline_text(
            &store, "ses_1", &[], 80, selected_color, &agents, &model_names, "", false, &theme,
        );

        let flat = flatten(&render.text);
        assert!(flat.contains("General"), "agent name shown: {flat}");
        assert!(flat.contains("Model X"), "model display name shown: {flat}");
        assert!(flat.contains("[high]"), "variant shown: {flat}");

        let marker = render
            .text
            .0
            .iter()
            .flat_map(|line| line.0.iter())
            .find(|span| span.text.starts_with('\u{25a3}'))
            .expect("footer marker span");
        let producing = prompt_box::agent_color(&theme, &agents, Some("general"));
        assert_eq!(marker.fg, Some(producing), "marker uses producing agent color");
        assert_ne!(
            marker.fg,
            Some(selected_color),
            "marker is not the selected agent color"
        );
    }

    #[test]
    fn last_assistant_message_text_when_assistant_has_text_parts_joins_and_trims() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": " first " } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_2", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "second " } }),
        ));

        assert_eq!(
            last_assistant_message_text(&store, "ses_1"),
            Some("first \nsecond".to_owned()),
        );
    }

    #[test]
    fn last_assistant_message_text_when_session_reverted_uses_assistant_before_revert() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "kept" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_3", "sessionID": "ses_1", "role": "assistant", "time": { "created": 3 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_3", "messageID": "msg_3", "sessionID": "ses_1", "type": "text", "text": "hidden" } }),
        ));
        store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_1", "revert": { "messageID": "msg_2" } } }),
        ));

        assert_eq!(
            last_assistant_message_text(&store, "ses_1"),
            Some("kept".to_owned()),
        );
    }

    #[test]
    fn last_assistant_message_text_when_no_assistant_returns_none() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));

        assert_eq!(last_assistant_message_text(&store, "ses_1"), None);
    }

    #[test]
    fn last_assistant_message_text_status_when_no_text_parts_reports_no_text_parts() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "reasoning", "text": "thinking" } }),
        ));

        assert!(matches!(
            last_assistant_message_text_status(&store, "ses_1"),
            LastAssistantMessageText::NoTextParts
        ));
    }

    #[test]
    fn last_assistant_message_text_status_when_text_trims_empty_reports_empty_text() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "  \n " } }),
        ));

        assert!(matches!(
            last_assistant_message_text_status(&store, "ses_1"),
            LastAssistantMessageText::EmptyText
        ));
    }

    #[test]
    fn timeline_when_store_has_user_and_assistant_renders_both() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "hi there" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_2", "sessionID": "ses_1", "role": "assistant", "time": { "created": 2 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_2", "messageID": "msg_2", "sessionID": "ses_1", "type": "text", "text": "hello back" } }),
        ));

        let theme = theme();
        let rendered =
            flatten(&timeline_text(&store, "ses_1", &[], 80, theme.border, &[], &[], "", false, &theme).text);
        assert!(rendered.contains("hi there"), "user line: {rendered}");
        assert!(
            rendered.contains("hello back"),
            "assistant line: {rendered}"
        );
    }

    #[test]
    fn pending_echo_hidden_once_user_message_in_store() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "ping" } }),
        ));
        let theme = theme();
        let rendered = flatten(
            &timeline_text(
                &store,
                "ses_1",
                &["ping".to_string()],
                80,
                theme.border,
                &[],
                &[],
                "",
                false,
                &theme,
            )
            .text,
        );
        assert_eq!(rendered.matches("ping").count(), 1, "no duplicate echo");
    }

    #[test]
    fn timeline_dialog_items_when_user_messages_exist_are_newest_first() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1_000 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "first\nprompt" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_2", "sessionID": "ses_1", "role": "assistant", "time": { "created": 2_000 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_2", "messageID": "msg_2", "sessionID": "ses_1", "type": "text", "text": "answer" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_3", "sessionID": "ses_1", "role": "user", "time": { "created": 65_000 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_3a", "messageID": "msg_3", "sessionID": "ses_1", "type": "text", "text": "hidden", "synthetic": true } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_3b", "messageID": "msg_3", "sessionID": "ses_1", "type": "text", "text": "second prompt" } }),
        ));

        assert_eq!(
            timeline_dialog_items(&store, "ses_1"),
            vec![
                (
                    "msg_3".to_owned(),
                    "second prompt".to_owned(),
                    "00:01".to_owned(),
                ),
                (
                    "msg_1".to_owned(),
                    "first prompt".to_owned(),
                    "00:00".to_owned(),
                ),
            ],
        );
    }

    #[test]
    fn timeline_text_when_two_user_turns_reports_user_block_offsets() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "first" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_2", "sessionID": "ses_1", "role": "assistant", "time": { "created": 2 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_2", "messageID": "msg_2", "sessionID": "ses_1", "type": "text", "text": "answer" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_3", "sessionID": "ses_1", "role": "user", "time": { "created": 3 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_3", "messageID": "msg_3", "sessionID": "ses_1", "type": "text", "text": "second" } }),
        ));

        let theme = theme();
        let rendered = timeline_text(&store, "ses_1", &[], 80, theme.border, &[], &[], "", false, &theme);

        assert_eq!(
            rendered.message_offsets,
            vec![("msg_1".to_owned(), 0), ("msg_3".to_owned(), 5)],
        );
    }

    #[test]
    fn tool_part_renders_bash_command_and_output() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/live_tool_turn.jsonl"
        );
        let raw = std::fs::read_to_string(path).expect("fixtures/live_tool_turn.jsonl missing");
        let mut store = MessageStore::default();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let parsed: GlobalEvent = serde_json::from_str(line).expect("parse fixture line");
            if parsed.is_sync_envelope() || parsed.is_heartbeat() {
                continue;
            }
            store.apply_event(&parsed);
        }
        let session_id = store.messages.keys().next().expect("a session").clone();
        let theme = theme();
        let rendered = flatten(
            &timeline_text(
                &store,
                &session_id,
                &[],
                80,
                theme.border,
                &[],
                &[],
                "",
                false,
                &theme,
            )
            .text,
        );
        assert!(
            rendered.contains("$ echo hello-tool-output"),
            "bash command missing: {rendered}"
        );
        assert!(
            rendered.contains("hello-tool-output"),
            "bash output missing: {rendered}"
        );
    }
}
