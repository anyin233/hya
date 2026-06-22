use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::sidebar_format::workdir_label;
use crate::AppState;
use crate::theme::Theme;

const FOOTER_PADDING_WIDTH: usize = 2;

pub fn sidebar_footer_lines(state: &AppState, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(workdir_footer_line(state, theme, width));
    lines.push(Line::from(vec![
        Span::styled("  • ", Style::default().fg(theme.success)),
        Span::styled(
            format!("yaca {}", env!("CARGO_PKG_VERSION")),
            Style::default().fg(theme.muted),
        ),
    ]));
    lines
}

fn workdir_footer_line(state: &AppState, theme: &Theme, width: u16) -> Line<'static> {
    let label = workdir_footer_label(state, width);
    let split = label.rfind('/').map_or(0, |idx| idx + 1);
    let (parent, name) = label.split_at(split);
    Line::from(vec![
        Span::raw("  "),
        Span::styled(parent.to_string(), Style::default().fg(theme.muted)),
        Span::styled(name.to_string(), Style::default().fg(theme.text)),
    ])
}

fn workdir_footer_label(state: &AppState, width: u16) -> String {
    let workdir = workdir_label(state.projection.session.workdir.as_deref());
    let label = match state
        .branch_label
        .as_deref()
        .filter(|branch| !branch.is_empty())
    {
        Some(branch) => format!("{workdir}:{branch}"),
        None => workdir,
    };
    let content_width = usize::from(width).saturating_sub(FOOTER_PADDING_WIDTH);
    ellipsize_tail(&label, content_width)
}

fn ellipsize_tail(label: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(label) <= max_width {
        return label.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let tail_width = max_width - 1;
    let mut tail = Vec::new();
    let mut width = 0;
    for ch in label.chars().rev() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > tail_width {
            break;
        }
        width += ch_width;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}
