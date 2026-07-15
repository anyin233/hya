//! Shared prompt box, a port of `component/prompt/index.tsx`'s box: a left-bar
//! (`┃`) + bottom-underline (`╹▀▀▀`) frame over a `backgroundElement` content
//! panel, an agent/model meta row, and a hints row (`tab agents  ctrl+p commands`).

use ratatui::layout::Alignment;
use ratatui::style::Style;
use ratatui::text::{Line as RtLine, Span as RtSpan};
use ratatui::widgets::{Block, Paragraph};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::contracts::{Rect, Rgba};
use crate::prompt::display::prompt_offset_width;
use crate::render::draw::{rect_to_ratatui, rgba_to_color, text_to_ratatui};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::ResolvedTheme;

const BAR: &str = "┃";
const CORNER: &str = "╹";
const PAD: u16 = 2;
const MAX_TEXTAREA_ROWS: u16 = 6;

/// Agent-color palette, mirroring `context/local.tsx` `colors()`.
#[must_use]
pub fn agent_color(theme: &ResolvedTheme, agents: &[String], agent: Option<&str>) -> Rgba {
    let Some(agent) = agent else {
        return theme.border;
    };
    let palette = [
        theme.secondary,
        theme.accent,
        theme.success,
        theme.warning,
        theme.primary,
        theme.error,
        theme.info,
    ];
    match agents.iter().position(|name| name == agent) {
        Some(index) => palette[index % palette.len()],
        None => palette[0],
    }
}

#[must_use]
pub fn titlecase(name: &str) -> String {
    name.split_inclusive(|c: char| !c.is_alphanumeric())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

pub struct PromptBoxView<'a> {
    pub text: &'a str,
    pub placeholder: &'a str,
    pub agent_label: Option<&'a str>,
    pub agent_color: Rgba,
    pub model_label: Option<&'a str>,
    pub provider_label: Option<&'a str>,
    pub shell_mode: bool,
    pub working: bool,
    pub spinner: &'a str,
    pub agent_shortcut: &'a str,
    pub palette_shortcut: &'a str,
    /// Cursor byte offset into `text` (char boundary).
    pub cursor: usize,
    pub show_cursor: bool,
    pub yolo: bool,
}

/// Screen rects of the prompt box's clickable affordances, returned by [`draw`] so the runtime
/// can route mouse clicks (open agents/commands/model selectors) without re-deriving layout.
#[derive(Default, Clone, Copy)]
pub struct PromptHits {
    pub agents: Option<Rect>,
    pub commands: Option<Rect>,
    pub model: Option<Rect>,
}

#[must_use]
fn cursor_cell(text: &str, cursor: usize, text_w: u16) -> Option<(u16, u16)> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let target = usize::from(text_w.max(1));
    let mut row = 0u16;
    let mut col = 0usize;
    for (index, grapheme) in text.grapheme_indices(true) {
        if index == cursor {
            return Some(cursor_before_grapheme(row, col, grapheme, target, text_w));
        }
        advance_cursor(&mut row, &mut col, grapheme, target);
    }
    if cursor != text.len() {
        return None;
    }
    let col = u16::try_from(col)
        .unwrap_or(u16::MAX)
        .min(text_w.saturating_sub(1));
    Some((col, row))
}

#[must_use]
pub fn textarea_rows(text: &str, text_width: u16) -> u16 {
    wrapped_rows(text, text_width).min(MAX_TEXTAREA_ROWS)
}

#[must_use]
pub fn box_height(text: &str, area_width: u16) -> u16 {
    textarea_rows(text, text_area_width(area_width)) + 5
}

fn text_area_width(area_width: u16) -> u16 {
    area_width.saturating_sub(1 + PAD + PAD)
}

fn wrapped_rows(text: &str, text_width: u16) -> u16 {
    u16::try_from(input_text(text, Rgba::TRANSPARENT, text_width).0.len())
        .unwrap_or(u16::MAX)
        .max(1)
}

fn cursor_before_grapheme(
    row: u16,
    col: usize,
    grapheme: &str,
    target: usize,
    text_w: u16,
) -> (u16, u16) {
    if grapheme != "\n" && col > 0 && col + prompt_offset_width(grapheme) > target {
        return (0, row.saturating_add(1));
    }
    let col = u16::try_from(col)
        .unwrap_or(u16::MAX)
        .min(text_w.saturating_sub(1));
    (col, row)
}

fn advance_cursor(row: &mut u16, col: &mut usize, grapheme: &str, target: usize) {
    if grapheme == "\n" {
        *row = row.saturating_add(1);
        *col = 0;
        return;
    }
    let width = prompt_offset_width(grapheme);
    if *col > 0 && *col + width > target {
        *row = row.saturating_add(1);
        *col = 0;
    }
    *col += width;
}

fn viewport_start(total_rows: u16, cursor_row: u16, visible_rows: u16) -> u16 {
    if total_rows <= visible_rows {
        return 0;
    }
    cursor_row
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(total_rows - visible_rows)
}

fn visible_text(text: &Text, start: u16, rows: u16) -> Text {
    Text(
        text.0
            .iter()
            .skip(usize::from(start))
            .take(usize::from(rows))
            .cloned()
            .collect(),
    )
}

fn input_text(text: &str, color: Rgba, text_width: u16) -> Text {
    let target = usize::from(text_width.max(1));
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            lines.push(Line(std::mem::take(&mut spans)));
            width = 0;
            continue;
        }
        let grapheme_width = prompt_offset_width(grapheme);
        if width > 0 && width + grapheme_width > target {
            lines.push(Line(std::mem::take(&mut spans)));
            width = 0;
        }
        spans.push(Span::styled(grapheme, Some(color), None, Attrs::default()));
        width += grapheme_width;
    }
    if lines.is_empty() || !spans.is_empty() || text.ends_with('\n') {
        lines.push(Line(spans));
    }
    Text(lines)
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    view: &PromptBoxView<'_>,
    theme: &ResolvedTheme,
) -> PromptHits {
    if area.width < 4 || area.height < 6 {
        return PromptHits::default();
    }
    let bg = theme.background;
    let bg_el = theme.background_element;
    let panel_bg = Style::default().bg(rgba_to_color(bg_el, bg));
    let text_w = text_area_width(area.width);
    let rows = textarea_rows(view.text, text_w).min(area.height.saturating_sub(5).max(1));
    let cursor = cursor_cell(view.text, view.cursor.min(view.text.len()), text_w);
    let total_rows = wrapped_rows(view.text, text_w);
    let viewport_start = cursor.map_or(0, |(_, row)| viewport_start(total_rows, row, rows));
    let content_rows = rows + 3;

    let content_bg = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width - 1,
        height: content_rows,
    };
    frame.render_widget(
        Block::default().style(panel_bg),
        rect_to_ratatui(content_bg),
    );

    let bar_style = Style::default().fg(rgba_to_color(view.agent_color, bg));
    for row in 0..content_rows {
        let cell = Rect {
            x: area.x,
            y: area.y + row,
            width: 1,
            height: 1,
        };
        frame.render_widget(Paragraph::new(BAR).style(bar_style), rect_to_ratatui(cell));
    }

    let text_x = area.x + 1 + PAD;
    let (body, body_color) = if view.text.is_empty() {
        (view.placeholder, theme.text_muted)
    } else {
        (view.text, theme.text)
    };
    let body = input_text(body, body_color, text_w);
    let body = if view.text.is_empty() {
        body
    } else {
        visible_text(&body, viewport_start, rows)
    };
    let body = text_to_ratatui(&body, bg_el);
    frame.render_widget(
        Paragraph::new(body).style(panel_bg),
        rect_to_ratatui(Rect {
            x: text_x,
            y: area.y + 1,
            width: text_w,
            height: rows,
        }),
    );

    if view.show_cursor {
        if let Some((col, row)) = cursor {
            let row = row
                .saturating_sub(viewport_start)
                .min(rows.saturating_sub(1));
            frame.set_cursor_position((text_x + col, area.y + 1 + row));
        }
    }

    frame.render_widget(
        Paragraph::new(meta_line(view, theme)).style(panel_bg),
        rect_to_ratatui(Rect {
            x: text_x,
            y: area.y + rows + 2,
            width: text_w,
            height: 1,
        }),
    );

    let underline_y = area.y + content_rows;
    frame.render_widget(
        Paragraph::new(CORNER).style(bar_style),
        rect_to_ratatui(Rect {
            x: area.x,
            y: underline_y,
            width: 1,
            height: 1,
        }),
    );
    let underline: String = "▀".repeat((area.width - 1) as usize);
    frame.render_widget(
        Paragraph::new(underline).style(Style::default().fg(rgba_to_color(bg_el, bg))),
        rect_to_ratatui(Rect {
            x: area.x + 1,
            y: underline_y,
            width: area.width - 1,
            height: 1,
        }),
    );

    let hints_y = area.y + rows + 4;
    frame.render_widget(
        Paragraph::new(hints_line(view, theme)).alignment(Alignment::Right),
        rect_to_ratatui(Rect {
            x: area.x,
            y: hints_y,
            width: area.width.saturating_sub(PAD),
            height: 1,
        }),
    );
    if view.working {
        frame.render_widget(
            Paragraph::new(status_line(view, theme)).alignment(Alignment::Left),
            rect_to_ratatui(Rect {
                x: area.x + 1,
                y: hints_y,
                width: area.width.saturating_sub(1),
                height: 1,
            }),
        );
    }

    prompt_hits(view, area, text_x, rows, hints_y)
}

fn prompt_hits(
    view: &PromptBoxView<'_>,
    area: Rect,
    text_x: u16,
    rows: u16,
    hints_y: u16,
) -> PromptHits {
    if view.shell_mode {
        return PromptHits::default();
    }
    let agent_seg_w = UnicodeWidthStr::width(view.agent_shortcut) as u16 + 1 + 6;
    let cmd_seg_w = UnicodeWidthStr::width(view.palette_shortcut) as u16 + 1 + 8;
    let gap: u16 = 2;
    let right_edge = area.x + area.width.saturating_sub(PAD);
    let start_x = right_edge.saturating_sub(agent_seg_w + gap + cmd_seg_w);
    let model = view
        .agent_label
        .zip(view.model_label)
        .map(|(agent, model)| {
            let agent_w = UnicodeWidthStr::width(agent) as u16;
            Rect {
                x: text_x + agent_w + 3,
                y: area.y + rows + 2,
                width: UnicodeWidthStr::width(model) as u16,
                height: 1,
            }
        });
    PromptHits {
        agents: Some(Rect {
            x: start_x,
            y: hints_y,
            width: agent_seg_w,
            height: 1,
        }),
        commands: Some(Rect {
            x: start_x + agent_seg_w + gap,
            y: hints_y,
            width: cmd_seg_w,
            height: 1,
        }),
        model,
    }
}

fn status_line<'a>(view: &PromptBoxView<'a>, theme: &ResolvedTheme) -> RtLine<'a> {
    let bg = theme.background;
    RtLine::from(vec![
        RtSpan::styled(
            view.spinner.to_owned(),
            Style::default().fg(rgba_to_color(view.agent_color, bg)),
        ),
        RtSpan::raw(" Running  "),
        RtSpan::styled("esc", Style::default().fg(rgba_to_color(theme.text, bg))),
        RtSpan::raw(" "),
        RtSpan::styled(
            "interrupt",
            Style::default().fg(rgba_to_color(theme.text_muted, bg)),
        ),
    ])
}

fn meta_line<'a>(view: &PromptBoxView<'a>, theme: &ResolvedTheme) -> RtLine<'a> {
    let bg = theme.background;
    let bg_el = theme.background_element;
    let panel = Style::default().bg(rgba_to_color(bg_el, bg));
    let Some(agent) = view.agent_label else {
        return RtLine::default();
    };
    let mut spans = vec![RtSpan::styled(
        agent.to_owned(),
        panel.fg(rgba_to_color(view.agent_color, bg_el)),
    )];
    if !view.shell_mode {
        if let Some(model) = view.model_label {
            spans.push(RtSpan::styled(" ", panel));
            spans.push(RtSpan::styled(
                "·",
                panel.fg(rgba_to_color(theme.text_muted, bg_el)),
            ));
            spans.push(RtSpan::styled(" ", panel));
            spans.push(RtSpan::styled(
                model.to_owned(),
                panel.fg(rgba_to_color(theme.text, bg_el)),
            ));
            if let Some(provider) = view.provider_label {
                spans.push(RtSpan::styled(" ", panel));
                spans.push(RtSpan::styled(
                    provider.to_owned(),
                    panel.fg(rgba_to_color(theme.text_muted, bg_el)),
                ));
            }
        }
    }
    if view.yolo {
        spans.push(RtSpan::styled("  ", panel));
        spans.push(RtSpan::styled(
            "YOLO",
            panel.fg(rgba_to_color(theme.error, bg_el)),
        ));
    }
    RtLine::from(spans)
}

fn hints_line<'a>(view: &PromptBoxView<'a>, theme: &ResolvedTheme) -> RtLine<'a> {
    let bg = theme.background;
    let key = Style::default().fg(rgba_to_color(theme.text, bg));
    let label = Style::default().fg(rgba_to_color(theme.text_muted, bg));
    if view.shell_mode {
        return RtLine::from(vec![
            RtSpan::styled("esc", key),
            RtSpan::raw(" "),
            RtSpan::styled("exit shell mode", label),
        ]);
    }
    RtLine::from(vec![
        RtSpan::styled(view.agent_shortcut.to_owned(), key),
        RtSpan::raw(" "),
        RtSpan::styled("agents", label),
        RtSpan::raw("  "),
        RtSpan::styled(view.palette_shortcut.to_owned(), key),
        RtSpan::raw(" "),
        RtSpan::styled("commands", label),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    use crate::theme::{builtin_theme, resolve, Mode};

    fn theme() -> ResolvedTheme {
        let json = builtin_theme("hya").unwrap().unwrap();
        resolve(&json, Mode::Dark).unwrap()
    }

    #[test]
    fn box_height_grows_with_lines() {
        assert_eq!(box_height("", 80), 6);
        assert_eq!(box_height("one line", 80), 6);
        assert_eq!(box_height("a\nb", 80), 7);
        assert_eq!(box_height("a\nb\nc\nd\ne\nf\ng\nh", 80), 11);
        assert_eq!(box_height("abcdefghijklmnopqrstuvwxyz", 20), 7);
    }

    #[test]
    fn titlecase_capitalizes_each_word() {
        assert_eq!(titlecase("build"), "Build");
        assert_eq!(
            titlecase("Sisyphus - ultraworker"),
            "Sisyphus - Ultraworker"
        );
    }

    #[test]
    fn cursor_cell_tracks_row_col_and_cjk_width() {
        assert_eq!(cursor_cell("hello", 5, 20), Some((5, 0)));
        assert_eq!(cursor_cell("ab\ncd", 0, 20), Some((0, 0)));
        assert_eq!(cursor_cell("ab\ncd", 4, 20), Some((1, 1)));
        assert_eq!(cursor_cell("你好", 3, 20), Some((2, 0)));
        assert_eq!(cursor_cell("你好", 6, 20), Some((4, 0)));
        assert_eq!(cursor_cell("你好世界", 9, 6), Some((0, 1)));
        assert_eq!(cursor_cell("你好世界", 12, 6), Some((2, 1)));
        assert_eq!(cursor_cell("abcdefghijklmnop", 16, 15), Some((1, 1)));
        assert_eq!(cursor_cell("abcdefghijklmnoX", 15, 15), Some((0, 1)));
    }

    #[test]
    fn input_text_preserves_prompt_spaces_when_wrapping() {
        assert_eq!(
            text_rows(&input_text("  foo  ", Rgba::TRANSPARENT, 20)),
            ["  foo  "]
        );
        assert_eq!(
            text_rows(&input_text("     ", Rgba::TRANSPARENT, 3)),
            ["   ", "  "]
        );
        assert_eq!(
            text_rows(&input_text("a   ", Rgba::TRANSPARENT, 3)),
            ["a  ", " "]
        );
        assert_eq!(
            text_rows(&input_text("x\n  y  ", Rgba::TRANSPARENT, 20)),
            ["x", "  y  "]
        );
    }

    #[test]
    fn draw_when_user_input_soft_wraps_reserves_visible_rows() {
        let theme = theme();
        let text = "abcdefghijklmnopqrstuvwxyz";
        let view = PromptBoxView {
            text,
            placeholder: "Ask hya",
            agent_label: Some("build"),
            agent_color: theme.secondary,
            model_label: Some("dev"),
            provider_label: None,
            shell_mode: false,
            working: false,
            spinner: "",
            agent_shortcut: "tab",
            palette_shortcut: "ctrl+p",
            cursor: text.len(),
            show_cursor: true,
            yolo: false,
        };
        let width = 20;
        let height = 12;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    &view,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 1, width).contains("abcdefghijklmno"));
        assert!(
            row_text(buffer, 2, width).contains("pqrstuvwxyz"),
            "soft-wrapped input should reserve and render a second text row"
        );
    }

    #[test]
    fn draw_when_user_input_cjk_soft_wraps_reserves_visible_rows() {
        let theme = theme();
        let text = "你好世界";
        let view = PromptBoxView {
            text,
            placeholder: "Ask hya",
            agent_label: Some("build"),
            agent_color: theme.secondary,
            model_label: Some("dev"),
            provider_label: None,
            shell_mode: false,
            working: false,
            spinner: "",
            agent_shortcut: "tab",
            palette_shortcut: "ctrl+p",
            cursor: text.len(),
            show_cursor: true,
            yolo: false,
        };
        let width = 12;
        let height = 12;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    &view,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first_row = row_text(buffer, 1, width);
        assert!(first_row.contains('你'));
        assert!(first_row.contains('好'));
        assert!(first_row.contains('世'));
        assert!(
            row_text(buffer, 2, width).contains("界"),
            "wide CJK input should soft-wrap into a visible second text row"
        );
    }

    #[test]
    fn draw_when_user_input_has_spaces_preserves_editor_text() {
        let theme = theme();
        let text = "  foo  \n  bar";
        let view = PromptBoxView {
            text,
            placeholder: "Ask hya",
            agent_label: Some("build"),
            agent_color: theme.secondary,
            model_label: Some("dev"),
            provider_label: None,
            shell_mode: false,
            working: false,
            spinner: "",
            agent_shortcut: "tab",
            palette_shortcut: "ctrl+p",
            cursor: text.len(),
            show_cursor: true,
            yolo: false,
        };
        let width = 20;
        let height = 12;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    &view,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 1, width).contains("    foo  "));
        assert!(
            row_text(buffer, 2, width).contains("    bar"),
            "leading spaces after explicit newlines should remain visible in the editor"
        );
    }

    #[test]
    fn draw_when_user_input_exceeds_visible_rows_keeps_cursor_tail_visible() {
        let theme = theme();
        let text = [
            "000000000000000",
            "111111111111111",
            "222222222222222",
            "333333333333333",
            "444444444444444",
            "555555555555555",
            "666666666666666",
            "777777777777777",
        ]
        .concat();
        let view = PromptBoxView {
            text: &text,
            placeholder: "Ask hya",
            agent_label: Some("build"),
            agent_color: theme.secondary,
            model_label: Some("dev"),
            provider_label: None,
            shell_mode: false,
            working: false,
            spinner: "",
            agent_shortcut: "tab",
            palette_shortcut: "ctrl+p",
            cursor: text.len(),
            show_cursor: true,
            yolo: false,
        };
        let width = 20;
        let height = 14;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                draw(
                    frame,
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    &view,
                    &theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 1, width).contains("222222222222222"));
        assert!(
            row_text(buffer, 6, width).contains("777777777777777"),
            "the cursor-containing wrapped tail should be visible when input exceeds six rows"
        );
    }

    fn row_text(buffer: &Buffer, row: u16, width: u16) -> String {
        (0..width).map(|col| buffer[(col, row)].symbol()).collect()
    }

    fn text_rows(text: &Text) -> Vec<String> {
        text.0
            .iter()
            .map(|line| line.0.iter().map(|span| span.text.clone()).collect())
            .collect()
    }

    #[test]
    fn agent_color_uses_palette_index() {
        let theme = theme();
        let agents = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(agent_color(&theme, &agents, Some("a")), theme.secondary);
        assert_eq!(agent_color(&theme, &agents, Some("b")), theme.accent);
        assert_eq!(agent_color(&theme, &agents, None), theme.border);
    }
}
