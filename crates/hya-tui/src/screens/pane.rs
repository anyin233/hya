//! Read-only auxiliary pane + tab bar for the tmux-style multi-view (ADR-0003).
//!
//! An aux pane observes another agent's session live. It renders the SAME
//! transcript the main session screen does (via [`super::session::timeline_text`])
//! but with NO input bar and a `read-only` header — user input can never reach the
//! observed session. The tab bar advertises every open pane and which is focused.

use hya_sdk::MessageStore;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::render::draw;
use crate::render::scroll::ScrollState;
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::{selected_foreground, ResolvedTheme};

use super::prompt_box;
use super::session::timeline_text;

/// Everything an aux pane needs to render a read-only transcript.
pub struct AuxPaneView<'a> {
    /// The live message store (projection) to read the transcript from.
    pub store: &'a MessageStore,
    /// The observed session id.
    pub session_id: &'a str,
    /// The agent's stable handle (pane label).
    pub handle: &'a str,
    /// The agent's declared type, if known (from the roster).
    pub agent_type: Option<&'a str>,
    /// The agent's live roster status, if known (`idle`/`busy`/…).
    pub status: Option<&'a str>,
    /// Known agent names (for producer coloring parity with the session screen).
    pub agents: &'a [String],
    /// Known models (for the assistant footer model label).
    pub model_names: &'a [(String, String, String)],
    /// Current spinner frame for in-flight reasoning/tool parts.
    pub spinner: &'a str,
    /// Whether this pane currently has focus (affects the header accent).
    pub focused: bool,
}

/// Draw a read-only aux pane into `area`, scrolling its transcript with `scroll`.
pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    view: &AuxPaneView<'_>,
    scroll: &mut ScrollState,
    theme: &ResolvedTheme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let background = theme.background;
    frame.render_widget(
        Block::default().style(
            ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
        ),
        area,
    );

    let accent = if view.focused {
        theme.accent
    } else {
        theme.text_muted
    };
    let mut header = format!("\u{25b2} {}", view.handle);
    if let Some(agent_type) = view.agent_type.filter(|value| !value.is_empty()) {
        header.push_str(&format!(" \u{b7} {agent_type}"));
    }
    if let Some(status) = view.status.filter(|value| !value.is_empty()) {
        header.push_str(&format!(" \u{b7} {status}"));
    }
    let header_line = Line(vec![
        Span::styled(header, Some(accent), None, Attrs::default()),
        Span::styled(
            "  [read-only]",
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ),
    ]);
    let header_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(draw::text_to_ratatui(&Text(vec![header_line]), background)),
        header_area,
    );

    let body_area = Rect {
        x: area.x,
        y: area.y.saturating_add(1),
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    if body_area.height == 0 {
        return;
    }

    let agent_color = prompt_box::agent_color(theme, view.agents, Some(view.handle));
    let timeline = timeline_text(
        view.store,
        view.session_id,
        &[],
        body_area.width as usize,
        agent_color,
        view.agents,
        view.model_names,
        view.spinner,
        false,
        theme,
    );
    let old_height = scroll.content_height;
    scroll.viewport_height = body_area.height as usize;
    scroll.sticky_bottom(old_height, timeline.text.0.len());

    let body = draw::text_to_ratatui(&timeline.text, background);
    let messages = Paragraph::new(body)
        .scroll((scroll.offset as u16, 0))
        .wrap(Wrap { trim: false })
        .style(ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background)));
    frame.render_widget(messages, body_area);
}

/// Draw the one-row tab bar advertising every open pane and which is focused.
pub fn draw_tab_bar(
    frame: &mut ratatui::Frame<'_>,
    tabs: &[(String, bool)],
    theme: &ResolvedTheme,
) {
    if tabs.is_empty() {
        return;
    }
    let area = frame.area();
    let bar_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let background = theme.background;
    let mut spans = Vec::new();
    for (index, (label, focused)) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " ",
                Some(theme.text_muted),
                Some(theme.background_panel),
                Attrs::default(),
            ));
        }
        let (fg, bg) = if *focused {
            (selected_foreground(theme, Some(theme.accent)), theme.accent)
        } else {
            (theme.text_muted, theme.background_panel)
        };
        spans.push(Span::styled(
            format!(" {label} "),
            Some(fg),
            Some(bg),
            Attrs {
                bold: *focused,
                ..Attrs::default()
            },
        ));
    }
    frame.render_widget(
        Paragraph::new(draw::text_to_ratatui(&Text(vec![Line(spans)]), background)).style(
            ratatui::style::Style::default()
                .bg(draw::rgba_to_color(theme.background_panel, background)),
        ),
        bar_area,
    );
}
