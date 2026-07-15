use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear, Paragraph};
use serde_json::Value;

use hya_sdk::{MessageStore, Part};

use crate::contracts::Rgba;
use crate::render::draw::{rgba_to_color, text_to_ratatui};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::{selected_foreground, ResolvedTheme};

pub const OPTIONS: [(&str, &str); 3] = [
    ("once", "Allow once"),
    ("always", "Allow always"),
    ("reject", "Reject"),
];

pub const ALWAYS_OPTIONS: [(&str, &str); 2] = [("confirm", "Confirm"), ("cancel", "Cancel")];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    #[default]
    Permission,
    Always,
    Reject,
}

const MAX_BODY_LINES: usize = 8;
const MAX_HEIGHT: u16 = 16;
const FOOTER_OPTIONS_X: u16 = 2;
const FOOTER_LEADING_SPACE: u16 = 1;
const FOOTER_BUTTON_GAP: u16 = 1;

#[must_use]
pub fn tool_input(store: &MessageStore, request: &Value) -> Value {
    let tool = request.get("tool");
    let message_id = tool
        .and_then(|t| t.get("messageID"))
        .and_then(Value::as_str);
    let call_id = tool.and_then(|t| t.get("callID")).and_then(Value::as_str);
    let (Some(message_id), Some(call_id)) = (message_id, call_id) else {
        return Value::Null;
    };
    let Some(parts) = store.parts.get(message_id) else {
        return Value::Null;
    };
    for part in parts {
        if let Part::Tool(tool_part) = &part.inner {
            if tool_part.rest.get("callID").and_then(Value::as_str) == Some(call_id) {
                if let Some(input) = tool_part
                    .state
                    .as_ref()
                    .and_then(|state| state.get("input"))
                {
                    return input.clone();
                }
            }
        }
    }
    Value::Null
}

struct Info {
    icon: &'static str,
    title: String,
    body: Vec<Line>,
}

fn str_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn muted_line(prefix: &str, value: &str, theme: &ResolvedTheme) -> Vec<Line> {
    if value.is_empty() {
        return Vec::new();
    }
    vec![Line(vec![Span::styled(
        format!("{prefix}{value}"),
        Some(theme.text_muted),
        None,
        Attrs::default(),
    )])]
}

fn diff_lines(diff: &str, theme: &ResolvedTheme) -> Vec<Line> {
    diff.lines()
        .filter(|line| !line.starts_with("diff ") && !line.starts_with("index "))
        .take(MAX_BODY_LINES)
        .map(|line| {
            let color = match line.chars().next() {
                Some('+') => theme.diff_added,
                Some('-') => theme.diff_removed,
                Some('@') => theme.primary,
                _ => theme.text_muted,
            };
            Line(vec![Span::styled(
                line.to_owned(),
                Some(color),
                None,
                Attrs::default(),
            )])
        })
        .collect()
}

fn titlecase(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn info(request: &Value, input: &Value, theme: &ResolvedTheme) -> Info {
    let permission = request
        .get("permission")
        .and_then(Value::as_str)
        .unwrap_or("");
    let metadata = request.get("metadata").cloned().unwrap_or(Value::Null);
    match permission {
        "edit" => {
            let filepath = str_at(&metadata, "filepath");
            let diff = str_at(&metadata, "diff");
            let body = if diff.is_empty() {
                muted_line("Path: ", &filepath, theme)
            } else {
                diff_lines(&diff, theme)
            };
            Info {
                icon: "→",
                title: format!("Edit {filepath}"),
                body,
            }
        }
        "read" => {
            let path = str_at(input, "filePath");
            Info {
                icon: "→",
                title: format!("Read {path}"),
                body: muted_line("Path: ", &path, theme),
            }
        }
        "glob" => {
            let pattern = str_at(input, "pattern");
            Info {
                icon: "✱",
                title: format!("Glob \"{pattern}\""),
                body: muted_line("Pattern: ", &pattern, theme),
            }
        }
        "grep" => {
            let pattern = str_at(input, "pattern");
            Info {
                icon: "✱",
                title: format!("Grep \"{pattern}\""),
                body: muted_line("Pattern: ", &pattern, theme),
            }
        }
        "list" => {
            let path = str_at(input, "path");
            Info {
                icon: "→",
                title: format!("List {path}"),
                body: muted_line("Path: ", &path, theme),
            }
        }
        "bash" => {
            let description = str_at(input, "description");
            let command = str_at(input, "command");
            let title = if description.is_empty() {
                "Shell command".to_owned()
            } else {
                description
            };
            let body = if command.is_empty() {
                Vec::new()
            } else {
                vec![Line(vec![Span::styled(
                    format!("$ {command}"),
                    Some(theme.text),
                    None,
                    Attrs::default(),
                )])]
            };
            Info {
                icon: "#",
                title,
                body,
            }
        }
        "webfetch" => {
            let url = str_at(input, "url");
            Info {
                icon: "%",
                title: format!("WebFetch {url}"),
                body: muted_line("URL: ", &url, theme),
            }
        }
        "task" => {
            let subagent = str_at(input, "subagent_type");
            let description = str_at(input, "description");
            let body = if description.is_empty() {
                Vec::new()
            } else {
                vec![Line(vec![Span::styled(
                    format!("◉ {description}"),
                    Some(theme.text),
                    None,
                    Attrs::default(),
                )])]
            };
            Info {
                icon: "#",
                title: format!("{} Task", titlecase(&subagent)),
                body,
            }
        }
        "tool" => {
            let tool = request
                .get("patterns")
                .and_then(Value::as_array)
                .and_then(|patterns| patterns.first())
                .and_then(Value::as_str)
                .unwrap_or("tool");
            Info {
                icon: "⚙",
                title: format!("Call tool {tool}"),
                body: muted_line("Tool: ", tool, theme),
            }
        }
        other => Info {
            icon: "⚙",
            title: format!("Call tool {other}"),
            body: muted_line("Tool: ", other, theme),
        },
    }
}

fn bar(color: Rgba) -> Span {
    Span::styled("┃", Some(color), None, Attrs::default())
}

fn pad(n: usize) -> Span {
    Span::styled(" ".repeat(n), None, None, Attrs::default())
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    request: &Value,
    input: &Value,
    stage: Stage,
    selected: usize,
    reject_message: &str,
    theme: &ResolvedTheme,
) {
    match stage {
        Stage::Permission => {
            let info = info(request, input, theme);
            let mut lines = vec![
                Line(vec![bar(theme.warning)]),
                header_line(theme),
                Line(vec![
                    bar(theme.warning),
                    pad(3),
                    Span::styled(info.icon, Some(theme.text_muted), None, Attrs::default()),
                    pad(1),
                    Span::styled(info.title.clone(), Some(theme.text), None, Attrs::default()),
                ]),
            ];
            if !info.body.is_empty() {
                lines.push(Line(vec![bar(theme.warning)]));
                for body_line in info.body {
                    let mut spans = vec![bar(theme.warning), pad(2)];
                    spans.extend(body_line.0);
                    lines.push(Line(spans));
                }
            }
            render_prompt_box(frame, lines, &OPTIONS, selected, theme);
        }
        Stage::Always => {
            let mut lines = vec![
                Line(vec![bar(theme.warning)]),
                header_line(theme),
                Line(vec![
                    bar(theme.warning),
                    pad(3),
                    Span::styled("Always allow", Some(theme.text), None, Attrs::default()),
                ]),
                Line(vec![bar(theme.warning)]),
            ];
            if let Some(patterns) = request.get("always").and_then(Value::as_array) {
                for pattern in patterns
                    .iter()
                    .filter_map(Value::as_str)
                    .take(MAX_BODY_LINES)
                {
                    lines.push(Line(vec![
                        bar(theme.warning),
                        pad(2),
                        Span::styled(
                            format!("- {pattern}"),
                            Some(theme.text),
                            None,
                            Attrs::default(),
                        ),
                    ]));
                }
            }
            render_prompt_box(frame, lines, &ALWAYS_OPTIONS, selected, theme);
        }
        Stage::Reject => draw_reject(frame, reject_message, theme),
    }
}

fn header_line(theme: &ResolvedTheme) -> Line {
    Line(vec![
        bar(theme.warning),
        pad(1),
        Span::styled("△", Some(theme.warning), None, Attrs::default()),
        pad(1),
        Span::styled(
            "Permission required",
            Some(theme.text),
            None,
            Attrs::default(),
        ),
    ])
}

fn render_prompt_box(
    frame: &mut ratatui::Frame<'_>,
    lines: Vec<Line>,
    options: &[(&str, &str)],
    selected: usize,
    theme: &ResolvedTheme,
) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let height = (lines.len() as u16 + 2).min(MAX_HEIGHT).min(screen.height);
    let area = Rect {
        x: screen.x,
        y: screen.y + screen.height.saturating_sub(height),
        width: screen.width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);

    let content_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    let body = text_to_ratatui(&Text(lines), bg);
    frame.render_widget(
        Paragraph::new(body).style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel)),
        content_area,
    );

    draw_footer(frame, area, options, selected, theme);
}

fn draw_reject(frame: &mut ratatui::Frame<'_>, message: &str, theme: &ResolvedTheme) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let lines = vec![
        Line(vec![bar(theme.error)]),
        Line(vec![
            bar(theme.error),
            pad(1),
            Span::styled("△", Some(theme.error), None, Attrs::default()),
            pad(1),
            Span::styled(
                "Reject permission",
                Some(theme.text),
                None,
                Attrs::default(),
            ),
        ]),
        Line(vec![
            bar(theme.error),
            pad(3),
            Span::styled(
                "Tell HYA what to do differently",
                Some(theme.text_muted),
                None,
                Attrs::default(),
            ),
        ]),
        Line(vec![bar(theme.error)]),
        Line(vec![
            bar(theme.error),
            pad(2),
            Span::styled(
                format!("{message}▏"),
                Some(theme.text),
                None,
                Attrs::default(),
            ),
        ]),
    ];
    let height = (lines.len() as u16 + 2).min(MAX_HEIGHT).min(screen.height);
    let area = Rect {
        x: screen.x,
        y: screen.y + screen.height.saturating_sub(height),
        width: screen.width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);
    let content_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    let body = text_to_ratatui(&Text(lines), bg);
    frame.render_widget(
        Paragraph::new(body).style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel)),
        content_area,
    );

    let element = rgba_to_color(theme.background_element, bg);
    let footer_y = area.y + area.height.saturating_sub(1);
    let footer_area = Rect {
        x: area.x,
        y: footer_y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Block::default().style(Style::default().bg(element)),
        footer_area,
    );
    let hint = Line(vec![
        Span::styled("  enter", Some(theme.text), None, Attrs::default()),
        Span::styled(
            " confirm   ",
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ),
        Span::styled("esc", Some(theme.text), None, Attrs::default()),
        Span::styled(" cancel", Some(theme.text_muted), None, Attrs::default()),
    ]);
    frame.render_widget(
        Paragraph::new(text_to_ratatui(&Text(vec![hint]), bg)).style(Style::default().bg(element)),
        footer_area,
    );
}

fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    options: &[(&str, &str)],
    selected: usize,
    theme: &ResolvedTheme,
) {
    let bg = theme.background;
    let element = rgba_to_color(theme.background_element, bg);
    let footer_y = area.y + area.height.saturating_sub(1);
    let footer_area = Rect {
        x: area.x,
        y: footer_y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Block::default().style(Style::default().bg(element)),
        footer_area,
    );

    let mut option_spans: Vec<Span> = vec![Span::styled(
        " ",
        None,
        Some(theme.background_element),
        Attrs::default(),
    )];
    for (index, (_, label)) in options.iter().enumerate() {
        let (fg, button_bg): (Rgba, Rgba) = if index == selected {
            (
                selected_foreground(theme, Some(theme.warning)),
                theme.warning,
            )
        } else {
            (theme.text_muted, theme.background_menu)
        };
        option_spans.push(Span::styled(
            format!(" {label} "),
            Some(fg),
            Some(button_bg),
            Attrs::default(),
        ));
        option_spans.push(Span::styled(
            " ",
            None,
            Some(theme.background_element),
            Attrs::default(),
        ));
    }
    let options = text_to_ratatui(&Text(vec![Line(option_spans)]), bg);
    frame.render_widget(
        Paragraph::new(options).style(Style::default().bg(element)),
        Rect {
            x: footer_area.x + FOOTER_OPTIONS_X,
            y: footer_y,
            width: footer_area.width.saturating_sub(FOOTER_OPTIONS_X),
            height: 1,
        },
    );

    let hint = Line(vec![
        Span::styled("⇆", Some(theme.text), None, Attrs::default()),
        Span::styled(" select  ", Some(theme.text_muted), None, Attrs::default()),
        Span::styled("enter", Some(theme.text), None, Attrs::default()),
        Span::styled(" confirm", Some(theme.text_muted), None, Attrs::default()),
    ]);
    let hint_width = hint.width() as u16 + 3;
    if footer_area.width > hint_width + 24 {
        let hint_widget = text_to_ratatui(&Text(vec![hint]), bg);
        frame.render_widget(
            Paragraph::new(hint_widget)
                .alignment(Alignment::Right)
                .style(Style::default().bg(element)),
            Rect {
                x: footer_area.x + footer_area.width.saturating_sub(hint_width),
                y: footer_y,
                width: hint_width.saturating_sub(3),
                height: 1,
            },
        );
    }
}

pub(crate) fn permission_button_at(
    screen: Rect,
    stage: Stage,
    column: u16,
    row: u16,
) -> Option<usize> {
    if screen.height == 0 || row != screen.y + screen.height - 1 {
        return None;
    }
    let options: &[(&str, &str)] = match stage {
        Stage::Permission => &OPTIONS,
        Stage::Always => &ALWAYS_OPTIONS,
        Stage::Reject => return None,
    };
    footer_button_at(screen.x, options, column)
}

fn footer_button_at(area_x: u16, options: &[(&str, &str)], column: u16) -> Option<usize> {
    let mut x = area_x + FOOTER_OPTIONS_X + FOOTER_LEADING_SPACE;
    for (index, (_, label)) in options.iter().enumerate() {
        let width = label.len() as u16 + 2;
        if column >= x && column < x + width {
            return Some(index);
        }
        x += width + FOOTER_BUTTON_GAP;
    }
    None
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::{permission_button_at, Stage};

    #[test]
    fn permission_button_at_maps_permission_footer_cells() {
        let screen = Rect::new(0, 0, 80, 20);
        let row = screen.height - 1;

        assert_eq!(
            permission_button_at(screen, Stage::Permission, 3, row),
            Some(0)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 14, row),
            Some(0)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 16, row),
            Some(1)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 29, row),
            Some(1)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 31, row),
            Some(2)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 38, row),
            Some(2)
        );
    }

    #[test]
    fn permission_button_at_maps_always_footer_cells() {
        let screen = Rect::new(0, 0, 80, 20);
        let row = screen.height - 1;

        assert_eq!(permission_button_at(screen, Stage::Always, 3, row), Some(0));
        assert_eq!(
            permission_button_at(screen, Stage::Always, 11, row),
            Some(0)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Always, 13, row),
            Some(1)
        );
        assert_eq!(
            permission_button_at(screen, Stage::Always, 20, row),
            Some(1)
        );
    }

    #[test]
    fn permission_button_at_ignores_reject_stage_and_non_footer_cells() {
        let screen = Rect::new(0, 0, 80, 20);
        let row = screen.height - 1;

        assert_eq!(permission_button_at(screen, Stage::Reject, 3, row), None);
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 3, row - 1),
            None
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 2, row),
            None
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 15, row),
            None
        );
        assert_eq!(
            permission_button_at(screen, Stage::Permission, 30, row),
            None
        );
    }
}
