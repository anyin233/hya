//! Shared prompt box, a port of `component/prompt/index.tsx`'s box: a left-bar
//! (`┃`) + bottom-underline (`╹▀▀▀`) frame over a `backgroundElement` content
//! panel, an agent/model meta row, and a hints row (`tab agents  ctrl+p commands`).

use ratatui::layout::Alignment;
use ratatui::style::Style;
use ratatui::text::{Line as RtLine, Span as RtSpan};
use ratatui::widgets::{Block, Paragraph, Wrap};

use unicode_width::UnicodeWidthStr;

use crate::contracts::{Rect, Rgba};
use crate::render::draw::{rect_to_ratatui, rgba_to_color};
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
}

/// Screen rects of the prompt box's clickable affordances, returned by [`draw`] so the runtime
/// can route mouse clicks (open agents/commands/model selectors) without re-deriving layout.
#[derive(Default, Clone, Copy)]
pub struct PromptHits {
    pub agents: Option<Rect>,
    pub commands: Option<Rect>,
    pub model: Option<Rect>,
}

/// Cursor's `(col, row)` within the text area, by logical line (soft-wrap deferred).
#[must_use]
fn cursor_cell(text: &str, cursor: usize, text_w: u16, rows: u16) -> Option<(u16, u16)> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let line_start = before.rfind('\n').map_or(0, |byte| byte + 1);
    let row = u16::try_from(before.matches('\n').count())
        .unwrap_or(0)
        .min(rows.saturating_sub(1));
    let col = u16::try_from(UnicodeWidthStr::width(&text[line_start..cursor]))
        .unwrap_or(0)
        .min(text_w.saturating_sub(1));
    Some((col, row))
}

#[must_use]
pub fn textarea_rows(text: &str) -> u16 {
    (u16::try_from(text.matches('\n').count()).unwrap_or(MAX_TEXTAREA_ROWS) + 1)
        .clamp(1, MAX_TEXTAREA_ROWS)
}

#[must_use]
pub fn box_height(text: &str) -> u16 {
    textarea_rows(text) + 5
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
    let rows = textarea_rows(view.text);
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
    let text_w = area.width.saturating_sub(1 + PAD + PAD);
    let (body, body_color) = if view.text.is_empty() {
        (view.placeholder, theme.text_muted)
    } else {
        (view.text, theme.text)
    };
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .style(panel_bg.fg(rgba_to_color(body_color, bg_el))),
        rect_to_ratatui(Rect {
            x: text_x,
            y: area.y + 1,
            width: text_w,
            height: rows,
        }),
    );

    if view.show_cursor {
        if let Some((col, row)) =
            cursor_cell(view.text, view.cursor.min(view.text.len()), text_w, rows)
        {
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

fn prompt_hits(view: &PromptBoxView<'_>, area: Rect, text_x: u16, rows: u16, hints_y: u16) -> PromptHits {
    if view.shell_mode {
        return PromptHits::default();
    }
    let agent_seg_w = UnicodeWidthStr::width(view.agent_shortcut) as u16 + 1 + 6;
    let cmd_seg_w = UnicodeWidthStr::width(view.palette_shortcut) as u16 + 1 + 8;
    let gap: u16 = 2;
    let right_edge = area.x + area.width.saturating_sub(PAD);
    let start_x = right_edge.saturating_sub(agent_seg_w + gap + cmd_seg_w);
    let model = view.agent_label.zip(view.model_label).map(|(agent, model)| {
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
        RtSpan::raw(" "),
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
    use crate::theme::{builtin_theme, resolve, Mode};

    fn theme() -> ResolvedTheme {
        let json = builtin_theme("hya").unwrap().unwrap();
        resolve(&json, Mode::Dark).unwrap()
    }

    #[test]
    fn box_height_grows_with_lines() {
        assert_eq!(box_height(""), 6);
        assert_eq!(box_height("one line"), 6);
        assert_eq!(box_height("a\nb"), 7);
        assert_eq!(box_height("a\nb\nc\nd\ne\nf\ng\nh"), 11);
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
        assert_eq!(cursor_cell("hello", 5, 20, 3), Some((5, 0)));
        assert_eq!(cursor_cell("ab\ncd", 0, 20, 3), Some((0, 0)));
        assert_eq!(cursor_cell("ab\ncd", 4, 20, 3), Some((1, 1)));
        assert_eq!(cursor_cell("你好", 3, 20, 3), Some((2, 0)));
        assert_eq!(cursor_cell("你好", 6, 20, 3), Some((4, 0)));
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
