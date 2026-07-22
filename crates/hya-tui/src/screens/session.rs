use hya_sdk::{MemberProjection, MessageStore, Part, RosterEntry, StoredPart};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::contracts::{PromptDoc, Rgba};
use crate::keymap::default_binding_specs;
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
                let producer = message
                    .rest
                    .get("agent")
                    .and_then(serde_json::Value::as_str);
                let msg_color = prompt_box::agent_color(theme, agents, producer);
                push_assistant(&mut lines, parts, width, spinner, theme);
                push_assistant_footer(
                    &mut lines,
                    message,
                    messages,
                    width,
                    msg_color,
                    model_names,
                    theme,
                );
            }
            _ => {}
        }
    }
    push_member_activity_rows(
        &mut lines,
        store.members.get(session_id).map_or(&[][..], Vec::as_slice),
        width,
        theme,
    );
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

fn push_member_activity_rows(
    lines: &mut Vec<Line>,
    members: &[MemberProjection],
    _width: usize,
    theme: &ResolvedTheme,
) {
    for member in members
        .iter()
        .filter(|member| matches!(member.status.as_str(), "failed" | "cancelled"))
    {
        lines.push(member_activity_line(member, theme));
    }
}

fn member_activity_line(member: &MemberProjection, theme: &ResolvedTheme) -> Line {
    let color = status_color(&member.status, theme);
    Line(vec![
        Span::styled("◇ ", Some(color), None, Attrs::default()),
        Span::styled(
            member_activity_text(member),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ),
    ])
}

fn member_activity_text(member: &MemberProjection) -> String {
    let label = if member.subagent_type.is_empty() {
        "Subagent".to_owned()
    } else {
        format!("{} Subagent", prompt_box::titlecase(&member.subagent_type))
    };
    let mut text = format!("{label} spawned");
    if !member.description.is_empty() {
        text.push_str(" — ");
        text.push_str(&member.description);
    }
    if member.status != "spawning" {
        text.push_str(" · ");
        text.push_str(&member.status);
    }
    if !member.summary.is_empty() {
        text.push_str(" — ");
        text.push_str(&member.summary);
    }
    text
}

fn status_color(status: &str, theme: &ResolvedTheme) -> Rgba {
    match status {
        "done" => theme.success,
        "failed" | "cancelled" => theme.error,
        "busy" | "running" | "spawning" => theme.warning,
        _ => theme.text_muted,
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
    Parent { count: usize, attention: usize },
    Child { index: usize, total: usize },
}

pub(crate) fn subagent_status(store: &MessageStore, session_id: &str) -> Option<SubagentStatus> {
    if let Some(parent_id) = store
        .session(session_id)
        .and_then(|session| session.parent_id.as_deref())
    {
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
    let team_root = store.team_root_for(session_id);
    let mut roster_sessions = Vec::new();
    for entry in store
        .team_for(team_root)
        .into_iter()
        .flat_map(|team| team.roster.values())
    {
        let session = entry.session.as_str();
        if !session.is_empty() && session != team_root && !roster_sessions.contains(&session) {
            roster_sessions.push(session);
        }
    }

    if team_root != session_id {
        return roster_sessions
            .iter()
            .position(|session| *session == session_id)
            .map(|position| SubagentStatus::Child {
                index: position + 1,
                total: roster_sessions.len(),
            });
    }

    let count = roster_sessions.len();
    if count > 0 {
        let attention = roster_sessions
            .iter()
            .filter(|session| {
                !store.permissions(session).is_empty() || !store.questions(session).is_empty()
            })
            .count();
        return Some(SubagentStatus::Parent { count, attention });
    }

    let count = store.child_sessions(session_id).len();
    (count > 0).then_some(SubagentStatus::Parent {
        count,
        attention: 0,
    })
}

fn subagent_status_line(status: SubagentStatus, theme: &ResolvedTheme) -> Line {
    let (marker, marker_color, text) = match status {
        SubagentStatus::Parent { count, attention } => {
            let suffix = if attention > 0 {
                format!(" · {attention} attention")
            } else {
                String::new()
            };
            (
                "● ",
                theme.success,
                format!(
                    "{count} subagent{}{} · ctrl+x o manager",
                    if count == 1 { "" } else { "s" },
                    suffix
                ),
            )
        }
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

const NAVIGATE_SHORTCUTS: &[(&str, &str)] = &[
    ("session_background", "background"),
    ("session_child_first", "first"),
    ("session_child_cycle", "next"),
    ("session_child_cycle_reverse", "previous"),
    ("session_parent", "parent"),
];
const OPEN_SHORTCUTS: &[(&str, &str)] = &[
    ("pane_roster", "manager"),
    ("pane_open_tab", "tab"),
    ("pane_open_vertical", "vertical"),
    ("pane_open_horizontal", "horizontal"),
];
const PANE_SHORTCUTS: &[(&str, &str)] = &[
    ("pane_channels", "channels"),
    ("pane_close", "close"),
    ("pane_cycle", "cycle"),
    ("pane_focus_main", "main"),
];

pub(crate) fn roster_shortcut_dock(
    store: &MessageStore,
    session_id: &str,
    subagent: Option<SubagentStatus>,
    return_to_main: bool,
    width: usize,
    theme: &ResolvedTheme,
) -> Text {
    if subagent.is_none() && !return_to_main {
        return Text::default();
    }
    let mut lines = Vec::new();
    if let Some(status) = subagent {
        lines.push(subagent_status_line(status, theme));
    }
    if return_to_main {
        let escape = Span::styled(
            " · esc main",
            Some(theme.text_muted),
            None,
            Attrs::default(),
        );
        if let Some(line) = lines.last_mut() {
            line.0.push(escape);
        } else {
            lines.push(Line(vec![escape]));
        }
    }

    let team_root = store.team_root_for(session_id);
    for entry in store
        .team_for(team_root)
        .into_iter()
        .flat_map(|team| team.roster.values())
        .filter(|entry| !entry.session.is_empty() && entry.session != team_root)
    {
        let mut spans = vec![
            Span::styled(
                "● ",
                Some(status_color(&entry.status, theme)),
                None,
                Attrs::default(),
            ),
            Span::styled(
                entry.handle.as_str(),
                Some(theme.text),
                None,
                Attrs {
                    bold: true,
                    ..Attrs::default()
                },
            ),
        ];
        for value in [
            entry.agent_type.as_str(),
            entry.mode.as_str(),
            entry.status.as_str(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .chain(
            entry
                .current_task
                .as_deref()
                .filter(|value| !value.is_empty()),
        ) {
            spans.push(Span::styled(
                format!(" · {value}"),
                Some(theme.text_muted),
                None,
                Attrs::default(),
            ));
        }
        lines.push(Line(spans));
    }

    lines.push(shortcut_line("Navigate", NAVIGATE_SHORTCUTS, theme));
    lines.push(shortcut_line("Open", OPEN_SHORTCUTS, theme));
    lines.push(shortcut_line("Pane", PANE_SHORTCUTS, theme));
    Text(lines).wrap(width)
}

fn shortcut_line(title: &str, shortcuts: &[(&str, &str)], theme: &ResolvedTheme) -> Line {
    let mut spans = vec![Span::styled(
        title,
        Some(theme.text),
        None,
        Attrs {
            bold: true,
            ..Attrs::default()
        },
    )];
    for (config_key, label) in shortcuts {
        let Some((_, binding, _)) = default_binding_specs()
            .iter()
            .find(|(candidate, _, _)| candidate == config_key)
        else {
            continue;
        };
        spans.push(Span::styled(
            format!("  {binding}"),
            Some(theme.accent),
            None,
            Attrs::default(),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ));
    }
    Line(spans)
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
    pub yolo: bool,
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    view: &SessionView<'_>,
    scroll: &mut ScrollState,
    theme: &ResolvedTheme,
) -> prompt_box::PromptHits {
    draw_in_area(frame, frame.area(), view, scroll, theme)
}

pub fn draw_in_area(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
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
        yolo,
    } = *view;
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

    let desired_prompt_rows = prompt_box::box_height(&prompt.text, main_area.width);
    let min_prompt_rows = 6.min(main_area.height);
    let prompt_rows = desired_prompt_rows
        .min(main_area.height)
        .max(min_prompt_rows);
    let dock = roster_shortcut_dock(
        store,
        session_id,
        subagent,
        matches!(subagent, Some(SubagentStatus::Child { .. })),
        main_area.width as usize,
        theme,
    );
    let dock_rows = (dock.0.len().min(u16::MAX as usize) as u16)
        .min(main_area.height.saturating_sub(prompt_rows));
    let timeline_rows = main_area
        .height
        .saturating_sub(prompt_rows)
        .saturating_sub(dock_rows);
    let timeline_area = ratatui::layout::Rect {
        x: main_area.x,
        y: main_area.y,
        width: main_area.width,
        height: timeline_rows,
    };
    let dock_area = ratatui::layout::Rect {
        x: main_area.x,
        y: main_area.y.saturating_add(timeline_rows),
        width: main_area.width,
        height: dock_rows,
    };
    let prompt_chunk = ratatui::layout::Rect {
        x: main_area.x,
        y: main_area
            .y
            .saturating_add(timeline_rows)
            .saturating_add(dock_rows),
        width: main_area.width,
        height: prompt_rows,
    };

    let timeline_agent_color = prompt_box::agent_color(theme, agents, active_agent);
    let timeline = timeline_text(
        store,
        session_id,
        pending,
        timeline_area.width as usize,
        timeline_agent_color,
        agents,
        model_names,
        spinner,
        show_timestamps,
        theme,
    );
    let old_height = scroll.content_height;
    scroll.viewport_height = timeline_area.height as usize;
    scroll.sticky_bottom(old_height, timeline.text.0.len());

    let body = draw::text_to_ratatui(&timeline.text, background);
    let messages = Paragraph::new(body)
        .scroll((scroll.offset as u16, 0))
        .wrap(Wrap { trim: false })
        .style(ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background)));
    frame.render_widget(messages, timeline_area);

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
        yolo,
    };
    let rendered = draw::text_to_ratatui(&dock, background);
    frame.render_widget(
        Paragraph::new(rendered).style(
            ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
        ),
        dock_area,
    );

    let prompt_area = crate::contracts::Rect {
        x: prompt_chunk.x,
        y: prompt_chunk.y,
        width: prompt_chunk.width,
        height: prompt_chunk.height,
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

    push_roster_block(&mut lines, store, session_id, inner.width as usize, theme);
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

fn push_roster_block(
    lines: &mut Vec<Line>,
    store: &MessageStore,
    session_id: &str,
    width: usize,
    theme: &ResolvedTheme,
) {
    let team_root = store.team_root_for(session_id);
    let mut header_written = false;
    for entry in store
        .team_for(team_root)
        .into_iter()
        .flat_map(|team| team.roster.values())
        .filter(|entry| roster_entry_actionable(store, session_id, team_root, entry))
    {
        if !header_written {
            lines.push(Line::default());
            lines.push(Line(vec![Span::styled(
                "Subagents",
                Some(theme.text),
                None,
                bold(),
            )]));
            header_written = true;
        }
        let task = entry
            .current_task
            .as_deref()
            .filter(|task| !task.is_empty())
            .map(|task| format!(" — {task}"))
            .unwrap_or_default();
        let line = format!("{} {}{}", entry.handle, entry.status, task);
        let shown = truncate_right(&line, width);
        lines.push(Line(vec![Span::styled(
            shown,
            Some(status_color(&entry.status, theme)),
            None,
            Attrs::default(),
        )]));
    }
}

fn roster_entry_actionable(
    store: &MessageStore,
    session_id: &str,
    team_root: &str,
    entry: &RosterEntry,
) -> bool {
    if entry.session.is_empty() || entry.session == session_id || entry.session == team_root {
        return false;
    }
    let attention = !store.permissions(&entry.session).is_empty()
        || !store.questions(&entry.session).is_empty();
    matches!(entry.status.as_str(), "busy" | "failed") || attention
}

fn truncate_right(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    let keep = budget.saturating_sub(1);
    let head: String = text.chars().take(keep).collect();
    format!("{head}…")
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

fn assistant_duration(message: &hya_sdk::Message, messages: &[hya_sdk::Message]) -> Option<String> {
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
    use crate::render::transcript::{format_store_transcript, TranscriptOptions};
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};
    use hya_sdk::GlobalEvent;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn theme() -> ResolvedTheme {
        resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap()
    }

    fn event(kind: &str, properties: serde_json::Value) -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .unwrap()
    }
    fn team_event(event: serde_json::Value) -> GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": {
                "type": "hya.envelope",
                "properties": { "seq": 1, "event": event }
            }
        }))
        .unwrap()
    }

    fn render_sidebar_text(
        store: &MessageStore,
        session_id: &str,
        width: u16,
        height: u16,
    ) -> String {
        let theme = theme();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_sidebar(frame, frame.area(), store, session_id, None, &theme);
            })
            .unwrap();
        rows_text(terminal.backend().buffer(), 0, height, width)
    }

    fn flatten(text: &Text) -> String {
        text.0
            .iter()
            .flat_map(|line| line.0.iter().map(|span| span.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width).map(|col| buffer[(col, row)].symbol()).collect()
    }

    fn rows_text(buffer: &ratatui::buffer::Buffer, start: u16, end: u16, width: u16) -> String {
        (start..end)
            .map(|row| row_text(buffer, row, width))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn subagent_status_classifies_parentless_roster_member_as_child() {
        let mut store = MessageStore::default();
        assert!(store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_child" } }),
        )));
        assert!(store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_main",
            "agent_session": "ses_child",
            "handle": "reviewer-1",
            "agent_type": "reviewer",
            "mode": "resident"
        }))));

        assert!(matches!(
            subagent_status(&store, "ses_child"),
            Some(SubagentStatus::Child { index: 1, total: 1 })
        ));
    }

    #[test]
    fn draw_renders_shared_roster_shortcut_dock_at_80_and_120_columns() {
        let mut store = MessageStore::default();
        assert!(store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_main" } }),
        )));
        assert!(store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_child" } }),
        )));
        assert!(store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_main",
            "agent_session": "ses_child",
            "handle": "reviewer-1",
            "agent_type": "reviewer",
            "mode": "resident"
        }))));
        assert!(store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-1",
            "status": "busy",
            "current_task": "reviewing"
        }))));

        let theme = theme();
        let prompt = PromptDoc::default();
        let expected = [
            "1 subagent",
            "reviewer-1",
            "reviewer",
            "resident",
            "busy",
            "reviewing",
            "ctrl+b",
            "<leader>down",
            "right",
            "left",
            "up",
            "<leader>o",
            "<leader>T",
            "<leader>V",
            "<leader>S",
            "<leader>i",
            "<leader>w",
            "<leader>.",
            "<leader>0",
        ];

        for width in [80, 120] {
            let height = 30;
            let dock = roster_shortcut_dock(
                &store,
                "ses_main",
                subagent_status(&store, "ses_main"),
                false,
                width as usize,
                &theme,
            );
            assert!(
                dock.0.iter().all(|line| line.width() <= width as usize),
                "dock lines must fit width {width}"
            );
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut scroll = ScrollState::default();
            terminal
                .draw(|frame| {
                    draw(
                        frame,
                        &SessionView {
                            store: &store,
                            session_id: "ses_main",
                            pending: &[],
                            prompt: &prompt,
                            agents: &[],
                            model_names: &[],
                            active_agent: None,
                            model_label: None,
                            provider_label: None,
                            context_limit: None,
                            spinner: "",
                            show_timestamps: false,
                            sidebar_visible: false,
                            subagent: subagent_status(&store, "ses_main"),
                            show_cursor: true,
                            yolo: false,
                        },
                        &mut scroll,
                        &theme,
                    );
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            let rendered = rows_text(buffer, 0, height, width);
            for value in expected {
                assert!(
                    rendered.contains(value),
                    "{value:?} missing at width {width}; frame:\n{rendered}"
                );
            }
            assert!(
                !rendered.contains("esc main"),
                "root sessions must retain Esc interrupt semantics"
            );
            let prompt_rows = prompt_box::box_height(&prompt.text, width)
                .min(height)
                .max(6.min(height));
            let prompt_top = height - prompt_rows;
            assert!(
                row_text(buffer, prompt_top - 1, width).contains("<leader>0"),
                "dock should end directly above the prompt at width {width}"
            );
        }
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
        let model_names = vec![(
            "prov/model-x".to_owned(),
            "Model X".to_owned(),
            "Prov".to_owned(),
        )];
        let selected_color = prompt_box::agent_color(&theme, &agents, Some("build"));
        let render = timeline_text(
            &store,
            "ses_1",
            &[],
            80,
            selected_color,
            &agents,
            &model_names,
            "",
            false,
            &theme,
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
        assert_eq!(
            marker.fg,
            Some(producing),
            "marker uses producing agent color"
        );
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
        let rendered = flatten(
            &timeline_text(
                &store,
                "ses_1",
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
        assert!(rendered.contains("hi there"), "user line: {rendered}");
        assert!(
            rendered.contains("hello back"),
            "assistant line: {rendered}"
        );
    }

    #[test]
    fn draw_when_prompt_soft_wraps_reserves_parent_layout_height() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "transcript-visible" } }),
        ));

        let theme = theme();
        let agents = vec!["build".to_owned()];
        let prompt_text = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
        let prompt = PromptDoc {
            text: prompt_text.to_owned(),
            cursor: prompt_text.len(),
            ..PromptDoc::default()
        };
        let width = 60;
        let height = 20;
        let prompt_top = height - prompt_box::box_height(&prompt.text, width);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut scroll = ScrollState::default();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &SessionView {
                        store: &store,
                        session_id: "ses_1",
                        pending: &[],
                        prompt: &prompt,
                        agents: &agents,
                        model_names: &[],
                        active_agent: Some("build"),
                        model_label: Some("dev"),
                        provider_label: None,
                        context_limit: None,
                        spinner: "",
                        show_timestamps: false,
                        sidebar_visible: false,
                        subagent: None,
                        show_cursor: true,
                        yolo: false,
                    },
                    &mut scroll,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let timeline_text = rows_text(buffer, 0, prompt_top, width);
        let prompt_text = rows_text(buffer, prompt_top, height, width);
        assert!(
            timeline_text.contains("transcript-visible"),
            "transcript should remain in the timeline region"
        );
        assert!(
            !prompt_text.contains("transcript-visible"),
            "transcript should not overlap the reserved prompt region"
        );
        assert!(
            row_text(buffer, prompt_top + 1, width).contains("abcdefghijklmnopqrstuvwxyz"),
            "prompt body should begin in the reserved prompt region"
        );
        assert!(
            row_text(buffer, height - 1, width).contains("commands"),
            "soft-wrapped prompt should reserve enough parent height for the hints row"
        );
    }

    #[test]
    fn draw_when_session_is_working_shows_running_indicator_below_prompt() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        let theme = theme();
        let prompt = PromptDoc::default();
        let width = 60;
        let height = 12;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut scroll = ScrollState::default();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &SessionView {
                        store: &store,
                        session_id: "ses_1",
                        pending: &[],
                        prompt: &prompt,
                        agents: &[],
                        model_names: &[],
                        active_agent: None,
                        model_label: None,
                        provider_label: None,
                        context_limit: None,
                        spinner: "⠋",
                        show_timestamps: false,
                        sidebar_visible: false,
                        subagent: None,
                        show_cursor: true,
                        yolo: false,
                    },
                    &mut scroll,
                    &theme,
                );
            })
            .unwrap();

        let status = row_text(terminal.backend().buffer(), height - 1, width);
        assert!(
            status.contains("⠋ esc interrupt"),
            "working session should show a running indicator below the prompt; row: {status}"
        );
    }

    #[test]
    fn authoritative_idle_suppresses_interrupt_indicator_with_pending_prompt_history() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "session.status",
            serde_json::json!({ "sessionID": "ses_1", "status": { "type": "idle" } }),
        ));
        let theme = theme();
        let prompt = PromptDoc::default();
        let pending = vec!["submitted prompt".to_owned()];
        let width = 60;
        let height = 12;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut scroll = ScrollState::default();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &SessionView {
                        store: &store,
                        session_id: "ses_1",
                        pending: &pending,
                        prompt: &prompt,
                        agents: &[],
                        model_names: &[],
                        active_agent: None,
                        model_label: None,
                        provider_label: None,
                        context_limit: None,
                        spinner: "⠋",
                        show_timestamps: false,
                        sidebar_visible: false,
                        subagent: None,
                        show_cursor: true,
                        yolo: false,
                    },
                    &mut scroll,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let frame = rows_text(buffer, 0, height, width);
        assert!(
            frame.contains("submitted prompt"),
            "pending prompt history should remain visible; frame:\n{frame}"
        );
        let status = row_text(buffer, height - 1, width);
        assert!(
            !status.contains("esc interrupt"),
            "authoritative idle status should suppress the interrupt indicator; row: {status}"
        );
    }

    #[test]
    fn draw_when_terminal_too_short_clips_transcript_before_prompt() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "assistant", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "transcript-clipped" } }),
        ));

        let theme = theme();
        let agents = vec!["build".to_owned()];
        let prompt_text = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
        let prompt = PromptDoc {
            text: prompt_text.to_owned(),
            cursor: prompt_text.len(),
            ..PromptDoc::default()
        };
        let width = 60;
        let height = 6;
        assert!(
            prompt_box::box_height(&prompt.text, width) > height,
            "test prompt should want more rows than the short terminal height"
        );
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut scroll = ScrollState::default();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &SessionView {
                        store: &store,
                        session_id: "ses_1",
                        pending: &[],
                        prompt: &prompt,
                        agents: &agents,
                        model_names: &[],
                        active_agent: Some("build"),
                        model_label: Some("dev"),
                        provider_label: None,
                        context_limit: None,
                        spinner: "",
                        show_timestamps: false,
                        sidebar_visible: false,
                        subagent: None,
                        show_cursor: true,
                        yolo: false,
                    },
                    &mut scroll,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let frame_text = rows_text(buffer, 0, height, width);
        assert!(
            !frame_text.contains("transcript-clipped"),
            "transcript viewport should be clipped before the prompt on a short terminal; frame:\n{frame_text}"
        );
        assert!(
            row_text(buffer, height - 1, width).contains("commands"),
            "prompt composer should draw its hints inside the allocated prompt area; frame:\n{frame_text}"
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
        let rendered = timeline_text(
            &store,
            "ses_1",
            &[],
            80,
            theme.border,
            &[],
            &[],
            "",
            false,
            &theme,
        );

        assert_eq!(
            rendered.message_offsets,
            vec![("msg_1".to_owned(), 0), ("msg_3".to_owned(), 5)],
        );
    }

    #[test]
    fn timeline_shows_only_task_tool_entry_for_successful_subagent() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": {
                "id": "msg_tool",
                "sessionID": "ses_1",
                "role": "assistant",
                "time": { "created": 1, "completed": 2 }
            } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": {
                "id": "prt_tool",
                "messageID": "msg_tool",
                "sessionID": "ses_1",
                "type": "tool",
                "tool": "task",
                "state": {
                    "status": "completed",
                    "input": {
                        "subagent_type": "oracle",
                        "description": "review the plan"
                    }
                }
            } }),
        ));
        store.apply_event(&team_event(serde_json::json!({
            "type": "member_spawned",
            "session": "ses_1",
            "member": "mem_1",
            "child": "ses_child",
            "subagent_type": "oracle",
            "description": "review the plan",
            "depth": 1
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "member_finished",
            "session": "ses_1",
            "member": "mem_1",
            "status": "done",
            "summary": "looks good",
            "child": "ses_child"
        })));

        let theme = theme();
        let rendered = flatten(
            &timeline_text(
                &store,
                "ses_1",
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

        assert_eq!(
            rendered.matches("Oracle Task").count(),
            1,
            "expected one task entry: {rendered}"
        );
        assert_eq!(
            rendered.matches("review the plan").count(),
            1,
            "task description should appear only in the task entry: {rendered}"
        );
        assert!(!rendered.contains("spawned"), "extra spawn row: {rendered}");
        assert!(
            !rendered.contains("looks good"),
            "extra outcome row: {rendered}"
        );
    }

    #[test]
    fn issue21_timeline_renders_terminal_subagent_outcome_row() {
        let mut store = MessageStore::default();
        store.apply_event(&team_event(serde_json::json!({
            "type": "member_finished",
            "session": "ses_1",
            "member": "mem_1",
            "status": "failed",
            "summary": "needs approval",
            "child": "ses_child"
        })));

        let theme = theme();
        let rendered = timeline_text(
            &store,
            "ses_1",
            &[],
            120,
            theme.border,
            &[],
            &[],
            "",
            false,
            &theme,
        );
        let flattened = flatten(&rendered.text);

        assert_eq!(
            rendered.text.0.len(),
            1,
            "member_finished should render one compact terminal Subagent activity row"
        );
        assert!(
            flattened.contains("failed"),
            "terminal status missing: {flattened}"
        );
        assert!(
            flattened.contains("needs approval"),
            "terminal summary missing: {flattened}"
        );
    }

    #[test]
    fn successful_member_activity_does_not_add_timeline_rows_or_copy_task_text() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "start" } }),
        ));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_1",
            "agent_session": "ses_child",
            "handle": "oracle-1",
            "agent_type": "oracle",
            "mode": "transient"
        })));
        let theme = theme();
        let baseline = timeline_text(
            &store,
            "ses_1",
            &[],
            120,
            theme.border,
            &[],
            &[],
            "",
            false,
            &theme,
        )
        .text
        .0
        .len();

        store.apply_event(&team_event(serde_json::json!({
            "type": "member_spawned",
            "session": "ses_1",
            "member": "mem_1",
            "child": "ses_child",
            "subagent_type": "oracle",
            "description": "review the plan",
            "depth": 1
        })));
        let after_spawn = timeline_text(
            &store,
            "ses_1",
            &[],
            120,
            theme.border,
            &[],
            &[],
            "",
            false,
            &theme,
        );

        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_1",
            "handle": "oracle-1",
            "status": "busy",
            "current_task": "triaging"
        })));
        let after_running = timeline_text(
            &store,
            "ses_1",
            &[],
            120,
            theme.border,
            &[],
            &[],
            "",
            false,
            &theme,
        );
        let after_running_text = flatten(&after_running.text);

        assert_eq!(
            after_spawn.text.0.len(),
            baseline,
            "member_spawned should not add a separate transcript row"
        );
        assert_eq!(
            after_running.text.0.len(),
            after_spawn.text.0.len(),
            "running/current-task roster updates should not add another transcript row"
        );
        assert!(
            !after_running_text.contains("review the plan"),
            "member_spawned description should stay out of transcript copy: {after_running_text}"
        );
        assert!(
            !after_running_text.contains("triaging"),
            "agent_activity_changed current_task should stay out of transcript copy: {after_running_text}"
        );
    }

    #[test]
    fn successful_member_activity_stays_out_of_live_and_exported_transcripts() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_1", "title": "Main Session" } }),
        ));
        store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": { "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": { "created": 1 } } }),
        ));
        store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_1", "type": "text", "text": "ship it" } }),
        ));
        store.apply_event(&team_event(serde_json::json!({
            "type": "member_spawned",
            "session": "ses_1",
            "member": "mem_1",
            "child": "ses_child",
            "subagent_type": "oracle",
            "description": "review the plan",
            "depth": 1
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "member_finished",
            "session": "ses_1",
            "member": "mem_1",
            "status": "done",
            "summary": "looks good",
            "child": "ses_child"
        })));

        let theme = theme();
        let live = flatten(
            &timeline_text(
                &store,
                "ses_1",
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
        let transcript = format_store_transcript(&store, "ses_1", TranscriptOptions::default())
            .expect("stored transcript");

        assert!(
            !live.contains("review the plan"),
            "live timeline should omit successful spawn activity: {live}"
        );
        assert!(
            !live.contains("looks good"),
            "live timeline should omit successful outcome activity: {live}"
        );
        assert!(
            transcript.contains("ship it"),
            "stored message transcript missing user message: {transcript}"
        );
        assert!(
            !transcript.contains("review the plan"),
            "derived spawn rows must stay out of exported transcript: {transcript}"
        );
        assert!(
            !transcript.contains("looks good"),
            "derived finish summaries must stay out of exported transcript: {transcript}"
        );
    }

    #[test]
    fn issue22_sidebar_renders_actionable_roster_entries_and_filters_idle_done() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_main", "title": "Main Session" } }),
        ));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_main",
            "agent_session": "ses_main",
            "handle": "reviewer-main",
            "agent_type": "reviewer",
            "mode": "resident"
        })));
        for (handle, session) in [
            ("reviewer-1", "ses_busy"),
            ("reviewer-2", "ses_failed"),
            ("reviewer-3", "ses_idle"),
            ("reviewer-4", "ses_done"),
            ("reviewer-5", "ses_blocked"),
        ] {
            store.apply_event(&team_event(serde_json::json!({
                "type": "agent_registered",
                "session": "ses_main",
                "agent_session": session,
                "handle": handle,
                "agent_type": "reviewer",
                "mode": "resident"
            })));
        }
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-1",
            "status": "busy",
            "current_task": "triaging"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-2",
            "status": "failed",
            "current_task": "needs approval"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-main",
            "status": "busy",
            "current_task": "coordinating root"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-3",
            "status": "idle"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-4",
            "status": "done"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_main",
            "handle": "reviewer-5",
            "status": "idle",
            "current_task": "waiting approval"
        })));
        store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_other", "title": "Other Team" } }),
        ));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_other",
            "agent_session": "ses_other",
            "handle": "reviewer-other-root",
            "agent_type": "reviewer",
            "mode": "resident"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_other",
            "agent_session": "ses_other_failed",
            "handle": "reviewer-other-failed",
            "agent_type": "reviewer",
            "mode": "resident"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_other",
            "handle": "reviewer-other-root",
            "status": "busy",
            "current_task": "triaging other team"
        })));
        store.apply_event(&team_event(serde_json::json!({
            "type": "agent_activity_changed",
            "session": "ses_other",
            "handle": "reviewer-other-failed",
            "status": "failed",
            "current_task": "other team failure"
        })));
        store.apply_event(&event(
            "permission.asked",
            serde_json::json!({
                "id": "per_blocked",
                "sessionID": "ses_blocked",
                "permission": "edit",
                "patterns": ["src/main.rs"],
                "metadata": { "filepath": "src/main.rs" },
                "always": []
            }),
        ));

        let rendered = render_sidebar_text(&store, "ses_main", SIDEBAR_WIDTH, 18);

        assert!(
            rendered.contains("reviewer-1"),
            "busy roster entry missing: {rendered}"
        );
        assert!(
            rendered.contains("busy"),
            "busy roster status missing: {rendered}"
        );
        assert!(
            rendered.contains("triaging"),
            "busy roster task missing: {rendered}"
        );
        assert!(
            rendered.contains("reviewer-2"),
            "failed roster entry missing: {rendered}"
        );
        assert!(
            rendered.contains("failed"),
            "failed roster status missing: {rendered}"
        );
        assert!(
            rendered.contains("reviewer-5"),
            "user-blocked roster entry should stay visible in the compact sidebar: {rendered}"
        );
        assert!(
            rendered.contains("waiting approval"),
            "attention-needed roster task missing: {rendered}"
        );
        assert!(
            !rendered.contains("reviewer-3"),
            "idle entries without attention should stay out of the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("reviewer-4"),
            "done entries without attention should stay out of the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("reviewer-main"),
            "active/root session roster entry must not appear as a subagent in the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("coordinating root"),
            "active/root session task must not appear as a subagent task in the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("reviewer-other-root"),
            "unrelated team root roster entry must not appear in the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("triaging other team"),
            "unrelated team root task must not appear in the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("reviewer-other-failed"),
            "unrelated team failed roster entry must not appear in the compact sidebar: {rendered}"
        );
        assert!(
            !rendered.contains("other team failure"),
            "unrelated team failed task must not appear in the compact sidebar: {rendered}"
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
