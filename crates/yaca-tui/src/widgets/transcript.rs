use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use yaca_proto::Role;

use super::error::{display_system_error_segment, is_system_error_text};
use super::transcript_metadata::{AssistantBlockStatus, assistant_metadata_label};
use super::transcript_reasoning::push_reasoning_lines;
use super::transcript_scroll::{top_with_selection_visible, wrapped_row_range, wrapped_rows};
use super::transcript_text::text_from_parts;
use super::transcript_tools::push_tool_lines;
use crate::AppState;
use crate::theme::Theme;
use crate::view_model::{TimelinePart, timeline_items};

pub fn render_timeline(frame: &mut Frame, area: Rect, state: &mut AppState, theme: &Theme) {
    let inner_height = area.height.max(1);
    let inner_width = area.width.max(1);
    let timeline = timeline_lines(state, theme, inner_width);
    let total = wrapped_rows(&timeline.lines, inner_width);
    let max_back = total.saturating_sub(inner_height);
    state.scroll_back = state.scroll_back.min(max_back);
    let top = max_back.saturating_sub(state.scroll_back);
    let selected_message = state.selected_message;
    let selected_rows = if state.selected_message_scroll_anchor == selected_message {
        None
    } else {
        timeline
            .selected_message_range
            .map(|range| wrapped_row_range(&timeline.lines, range, inner_width))
    };
    let top = top_with_selection_visible(top, inner_height, max_back, selected_rows);
    state.scroll_back = max_back.saturating_sub(top);
    state.selected_message_scroll_anchor = selected_message;

    frame.render_widget(
        Paragraph::new(timeline.lines)
            .style(theme.base())
            .wrap(Wrap { trim: false })
            .scroll((top, 0)),
        area,
    );
}

struct TimelineLines {
    lines: Vec<Line<'static>>,
    selected_message_range: Option<Range<usize>>,
}

fn timeline_lines(state: &AppState, theme: &Theme, width: u16) -> TimelineLines {
    let mut lines = vec![Line::from("")];
    let mut selected_message_range = None;
    let items = timeline_items(&state.projection);
    let streaming_assistant_idx = items
        .last()
        .filter(|item| state.running && matches!(item.role, Role::Assistant))
        .map(|_| items.len().saturating_sub(1));
    for (idx, item) in items.iter().enumerate() {
        let selected = state.selected_message == Some(idx);
        let is_user = matches!(item.role, Role::User);
        let start = lines.len();
        match item.role {
            Role::User => user_lines(&item.parts, selected, theme, &mut lines),
            Role::Assistant => assistant_lines(
                &item.parts,
                selected,
                assistant_status(streaming_assistant_idx, idx, item.duration_ms),
                state,
                theme,
                &mut lines,
            ),
            Role::System => system_lines(&item.parts, selected, theme, &mut lines),
        }
        if selected {
            fill_selected_surface(&mut lines[start..], theme, width);
            selected_message_range = Some(start..lines.len());
        } else if is_user {
            fill_panel_surface(&mut lines[start..], theme, width);
        }
        if is_user {
            lines.push(Line::from(""));
        }
    }

    TimelineLines {
        lines,
        selected_message_range,
    }
}

fn assistant_status(
    streaming_assistant_idx: Option<usize>,
    idx: usize,
    duration_ms: Option<u64>,
) -> AssistantBlockStatus {
    if streaming_assistant_idx == Some(idx) {
        AssistantBlockStatus::Streaming
    } else {
        AssistantBlockStatus::Completed { duration_ms }
    }
}

fn fill_selected_surface(lines: &mut [Line<'static>], theme: &Theme, width: u16) {
    fill_surface(lines, theme.text, theme.block, width);
}

fn fill_panel_surface(lines: &mut [Line<'static>], theme: &Theme, width: u16) {
    fill_surface(lines, theme.text, theme.panel, width);
}

fn fill_surface(lines: &mut [Line<'static>], fg: Color, bg: Color, width: u16) {
    let width = usize::from(width.max(1));
    for line in lines {
        let line_width = line.width();
        let rows = line_width.div_ceil(width).max(1);
        let target_width = rows.saturating_mul(width);
        let fill_width = target_width.saturating_sub(line_width);
        if fill_width > 0 {
            line.spans.push(Span::styled(
                " ".repeat(fill_width),
                Style::default().fg(fg).bg(bg),
            ));
        }
    }
}

fn user_lines(
    parts: &[TimelinePart],
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let text = text_from_parts(parts);
    lines.push(Line::from(""));
    for segment in text.split('\n') {
        lines.push(Line::from(vec![
            Span::styled("▏ ", user_block_style(theme.primary, selected, theme)),
            Span::styled("  ", user_block_style(theme.muted, selected, theme)),
            Span::styled(
                segment.to_string(),
                user_block_style(theme.text, selected, theme),
            ),
        ]));
    }
    lines.push(Line::from(""));
}

fn assistant_lines(
    parts: &[TimelinePart],
    selected: bool,
    status: AssistantBlockStatus,
    state: &AppState,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let mut has_visible_part = false;
    let mut previous_was_tool = false;
    for part in parts {
        match part {
            TimelinePart::Text(text) => {
                for segment in text.trim().split('\n') {
                    lines.push(Line::from(vec![
                        Span::styled("   ", block_style(theme.muted, selected, theme)),
                        Span::styled(
                            segment.to_string(),
                            block_style(theme.text, selected, theme),
                        ),
                    ]));
                }
                has_visible_part = true;
                previous_was_tool = false;
            }
            TimelinePart::Reasoning(text) => {
                if !text.trim().is_empty() {
                    push_reasoning_lines(text, selected, theme, lines);
                    has_visible_part = true;
                    previous_was_tool = false;
                }
            }
            TimelinePart::Tool {
                name,
                label,
                input,
                status,
            } => {
                if has_visible_part && !previous_was_tool {
                    lines.push(Line::from(""));
                }
                push_tool_lines(name, label, input, status, selected, theme, lines);
                has_visible_part = true;
                previous_was_tool = true;
            }
        }
    }
    if has_visible_part {
        lines.push(Line::from(vec![
            Span::styled("   ", block_style(theme.muted, selected, theme)),
            Span::styled(
                assistant_metadata_label(state, status),
                block_style(theme.muted, selected, theme),
            ),
        ]));
    }
    lines.push(Line::from(""));
}

fn system_lines(
    parts: &[TimelinePart],
    selected: bool,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let text = text_from_parts(parts);
    let is_error = is_system_error_text(&text);
    for (idx, segment) in text.split('\n').enumerate() {
        if is_error {
            let label = if idx == 0 { "error " } else { "      " };
            let display = display_system_error_segment(segment);
            lines.push(Line::from(vec![
                Span::styled(
                    label.to_string(),
                    block_style(theme.error, selected, theme).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    display.to_string(),
                    block_style(theme.error, selected, theme),
                ),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("sys ", block_style(theme.muted, selected, theme)),
                Span::styled(
                    segment.to_string(),
                    block_style(theme.muted, selected, theme),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
}

fn block_style(fg: Color, selected: bool, theme: &Theme) -> Style {
    let bg = if selected {
        theme.block
    } else {
        theme.background
    };
    Style::default().fg(fg).bg(bg)
}

fn user_block_style(fg: Color, selected: bool, theme: &Theme) -> Style {
    let bg = if selected { theme.block } else { theme.panel };
    Style::default().fg(fg).bg(bg)
}
