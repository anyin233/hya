use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::AppState;
use crate::theme::Theme;

pub fn render_prompt(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let title = if state.yolo {
        " message — YOLO · Enter send · Tab yolo off · Ctrl-C clear/interrupt · F2 model "
    } else {
        " message — Enter send · Tab yolo · / or @ popup · Ctrl-C clear/interrupt · F2 model "
    };
    let widget = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.primary)),
        Span::styled(state.input.clone(), Style::default().fg(theme.text)),
    ]))
    .style(Style::default().fg(theme.text).bg(theme.panel))
    .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if state.running {
                theme.border_active
            } else {
                theme.border_subtle
            })),
    );
    frame.render_widget(widget, area);
}

#[must_use]
pub fn prompt_cursor(state: &AppState, area: Rect) -> Option<(u16, u16)> {
    if state.permission.is_some() || state.running {
        return None;
    }
    let typed = u16::try_from(state.input.chars().count()).unwrap_or(u16::MAX);
    let rightmost = area.x + area.width.saturating_sub(2);
    let cursor_x = (area.x + 3).saturating_add(typed).min(rightmost);
    Some((cursor_x, area.y + 1))
}

pub fn render_footer(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let text = if state.scroll_back > 0 {
        format!(
            "scroll {} · End to return · Ctrl-C clear/interrupt",
            state.scroll_back
        )
    } else if state.exit_armed {
        "Ctrl-C again to exit · type to cancel".to_string()
    } else if state.yolo {
        "YOLO mode · Tab disables auto-allow · / commands · @ references".to_string()
    } else {
        "PgUp/PgDn scroll · Tab yolo · / commands · @ references · F2 model".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.muted),
        )))
        .style(theme.base()),
        area,
    );
}
