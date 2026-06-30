use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::contracts::{BindingId, Key, KeyEvent};
use crate::render::draw::rgba_to_color;
use crate::theme::ResolvedTheme;

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    entries: &[(KeyEvent, BindingId)],
    theme: &ResolvedTheme,
) {
    if entries.is_empty() {
        return;
    }
    let bg = theme.background;
    let area = frame.area();

    let mut sorted: Vec<&(KeyEvent, BindingId)> = entries.iter().collect();
    sorted.sort_by_key(|entry| format_key(&entry.0));

    let spans: Vec<Span> = sorted
        .iter()
        .flat_map(|(event, command)| {
            [
                Span::styled(
                    format_key(event),
                    Style::default()
                        .fg(rgba_to_color(theme.primary, bg))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {}    ", command.0),
                    Style::default().fg(rgba_to_color(theme.text_muted, bg)),
                ),
            ]
        })
        .collect();

    let height = 6u16.min(area.height);
    let box_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height),
        width: area.width,
        height,
    };
    frame.render_widget(Clear, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(rgba_to_color(theme.border_active, bg)))
        .title(" leader ")
        .style(Style::default().bg(rgba_to_color(theme.background_panel, bg)));
    let paragraph = Paragraph::new(Line::from(spans))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, box_area);
}

fn format_key(event: &KeyEvent) -> String {
    let base = match &event.key {
        Key::Char(c) => c.to_string(),
        Key::Enter => "enter".to_owned(),
        Key::Esc => "esc".to_owned(),
        Key::Backspace => "bksp".to_owned(),
        Key::Tab => "tab".to_owned(),
        Key::BackTab => "shift+tab".to_owned(),
        Key::Up => "up".to_owned(),
        Key::Down => "down".to_owned(),
        Key::Left => "left".to_owned(),
        Key::Right => "right".to_owned(),
        Key::Home => "home".to_owned(),
        Key::End => "end".to_owned(),
        Key::PageUp => "pgup".to_owned(),
        Key::PageDown => "pgdn".to_owned(),
        Key::Delete => "del".to_owned(),
        Key::Insert => "ins".to_owned(),
        _ => "?".to_owned(),
    };
    let mut out = String::new();
    if event.ctrl {
        out.push_str("ctrl+");
    }
    if event.alt {
        out.push_str("alt+");
    }
    out.push_str(&base);
    out
}
