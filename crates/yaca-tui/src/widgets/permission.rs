use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use super::overlays::ellipsize;
use crate::theme::Theme;
use crate::{PermissionPrompt, PermissionPromptStage};

pub fn render_permission(frame: &mut Frame, prompt: &PermissionPrompt, theme: &Theme) {
    let area = frame.area();
    let footer_height = u16::from(area.height > 1);
    let height = 9u16.min(area.height.saturating_sub(footer_height));
    if height == 0 {
        return;
    }
    let width = area.width.saturating_sub(4).max(12);
    let y = area.y + area.height.saturating_sub(height + footer_height);
    let clear_rect = Rect {
        x: area.x,
        y,
        width: area.width,
        height,
    };
    let rect = Rect {
        x: area.x + 2,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, clear_rect);

    let inner_width = usize::from(width).saturating_sub(4);
    let feedback_width = inner_width.saturating_sub("█".len());
    let rail_style = Style::default().fg(theme.warning).bg(theme.element);
    let reject_rail_style = Style::default().fg(theme.error).bg(theme.element);
    let rail = || Span::styled("▏ ", rail_style);
    let reject_rail = || Span::styled("▏ ", reject_rail_style);
    let mut option_spans = vec![rail()];
    option_spans.extend(
        prompt
            .options()
            .iter()
            .enumerate()
            .flat_map(|(idx, label)| {
                let style = if idx == prompt.selected {
                    Style::default().fg(theme.background).bg(theme.warning)
                } else {
                    Style::default().fg(theme.muted)
                };
                vec![Span::styled(format!(" {label} "), style), Span::raw(" ")]
            }),
    );
    let lines = match prompt.stage {
        PermissionPromptStage::Permission => vec![
            header_line(
                rail(),
                theme,
                "Permission required",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(vec![
                rail(),
                Span::styled("→ ", Style::default().fg(theme.muted)),
                Span::styled(
                    format!("{} wants to run:", prompt.title),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                rail(),
                Span::raw(ellipsize(&prompt.detail, inner_width)),
            ]),
            Line::from(vec![rail()]),
            Line::from(option_spans),
            Line::from(vec![
                rail(),
                Span::styled("⇆ select · enter confirm", Style::default().fg(theme.muted)),
            ]),
        ],
        PermissionPromptStage::Always => vec![
            header_line(
                rail(),
                theme,
                "Always allow",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(vec![
                rail(),
                Span::styled(
                    format!("This will allow {} until yaca is restarted.", prompt.title),
                    Style::default().fg(theme.text),
                ),
            ]),
            Line::from(vec![
                rail(),
                Span::raw(ellipsize(&prompt.detail, inner_width)),
            ]),
            Line::from(vec![rail()]),
            Line::from(option_spans),
            Line::from(vec![
                rail(),
                Span::styled(
                    "⇆ select · enter confirm · esc cancel",
                    Style::default().fg(theme.muted),
                ),
            ]),
        ],
        PermissionPromptStage::Reject => vec![
            header_line(
                reject_rail(),
                theme,
                "Reject permission",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(vec![
                reject_rail(),
                Span::styled(
                    ellipsize("Tell OpenCode what to do differently", inner_width),
                    Style::default().fg(theme.muted),
                ),
            ]),
            Line::from(vec![reject_rail()]),
            Line::from(vec![
                reject_rail(),
                Span::styled(
                    ellipsize(&prompt.reply, feedback_width),
                    Style::default().fg(theme.text),
                ),
                Span::styled("█", Style::default().fg(theme.primary)),
            ]),
            Line::from(vec![
                reject_rail(),
                Span::styled(
                    "enter confirm · esc cancel",
                    Style::default().fg(theme.muted),
                ),
            ]),
        ],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.text).bg(theme.element))
            .wrap(Wrap { trim: false }),
        rect,
    );
}

fn header_line<'a>(
    rail: Span<'a>,
    theme: &Theme,
    title: &'static str,
    marker_style: Style,
) -> Line<'a> {
    Line::from(vec![
        rail,
        Span::styled("△", marker_style),
        Span::styled(
            format!(" {title}"),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ])
}
