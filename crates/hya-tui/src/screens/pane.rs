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
use super::session::{roster_shortcut_dock, subagent_status, timeline_text};

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
        Span::styled(header.clone(), Some(accent), None, Attrs::default()),
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

    let dock = roster_shortcut_dock(
        view.store,
        view.session_id,
        subagent_status(view.store, view.session_id),
        true,
        body_area.width as usize,
        theme,
    );
    let dock_rows = (dock.0.len().min(u16::MAX as usize) as u16).min(body_area.height);
    let timeline_area = Rect {
        height: body_area.height.saturating_sub(dock_rows),
        ..body_area
    };
    let dock_area = Rect {
        x: body_area.x,
        y: body_area.y.saturating_add(timeline_area.height),
        width: body_area.width,
        height: dock_rows,
    };

    let agent_color = prompt_box::agent_color(theme, view.agents, Some(view.handle));
    let timeline = timeline_text(
        view.store,
        view.session_id,
        &[],
        timeline_area.width as usize,
        agent_color,
        view.agents,
        view.model_names,
        view.spinner,
        false,
        theme,
    );
    let old_height = scroll.content_height;
    scroll.viewport_height = timeline_area.height as usize;
    scroll.sticky_bottom(old_height, timeline.text.0.len());
    if scroll.new_output {
        let header_line = Line(vec![
            Span::styled(header, Some(accent), None, Attrs::default()),
            Span::styled(
                "  [read-only]",
                Some(theme.text_muted),
                None,
                Attrs::default(),
            ),
            Span::styled(
                "  [new output]",
                Some(theme.warning),
                None,
                Attrs::default(),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(draw::text_to_ratatui(&Text(vec![header_line]), background)),
            header_area,
        );
    }

    let body = draw::text_to_ratatui(&timeline.text, background);
    let messages = Paragraph::new(body)
        .scroll((scroll.offset as u16, 0))
        .wrap(Wrap { trim: false })
        .style(ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background)));
    frame.render_widget(messages, timeline_area);
    frame.render_widget(
        Paragraph::new(draw::text_to_ratatui(&dock, background)).style(
            ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
        ),
        dock_area,
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::session::{roster_shortcut_dock, subagent_status};
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};
    use hya_sdk::GlobalEvent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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

    fn theme() -> ResolvedTheme {
        resolve(&builtin_theme(DEFAULT_THEME).unwrap().unwrap(), Mode::Dark).unwrap()
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width).map(|col| buffer[(col, row)].symbol()).collect()
    }

    #[test]
    fn draw_bottom_docks_shared_roster_shortcuts_without_composer() {
        let mut store = MessageStore::default();
        assert!(store.apply_event(&event(
            "session.updated",
            serde_json::json!({ "info": { "id": "ses_child" } }),
        )));
        assert!(store.apply_event(&event(
            "message.updated",
            serde_json::json!({ "info": {
                "id": "msg_child", "sessionID": "ses_child", "role": "assistant",
                "time": { "created": 1 }
            } }),
        )));
        assert!(store.apply_event(&event(
            "message.part.updated",
            serde_json::json!({ "part": {
                "id": "prt_child", "messageID": "msg_child", "sessionID": "ses_child",
                "type": "text", "text": "child transcript"
            } }),
        )));
        assert!(store.apply_event(&team_event(serde_json::json!({
            "type": "agent_registered",
            "session": "ses_main",
            "agent_session": "ses_child",
            "handle": "reviewer-1",
            "agent_type": "reviewer",
            "mode": "resident"
        }))));

        let theme = theme();
        let expected_bindings = [
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
            let height = 24;
            let dock = roster_shortcut_dock(
                &store,
                "ses_child",
                subagent_status(&store, "ses_child"),
                true,
                width as usize,
                &theme,
            );
            assert!(dock.0.iter().all(|line| line.width() <= width as usize));
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut scroll = ScrollState::default();
            terminal
                .draw(|frame| {
                    draw(
                        frame,
                        frame.area(),
                        &AuxPaneView {
                            store: &store,
                            session_id: "ses_child",
                            handle: "reviewer-1",
                            agent_type: Some("reviewer"),
                            status: Some("busy"),
                            agents: &[],
                            model_names: &[],
                            spinner: "",
                            focused: true,
                        },
                        &mut scroll,
                        &theme,
                    );
                })
                .unwrap();

            let buffer = terminal.backend().buffer();
            let rendered = (0..height)
                .map(|row| row_text(buffer, row, width))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rendered.contains("child transcript"));
            assert!(rendered.contains("reviewer-1"));
            assert!(rendered.contains("esc main"));
            for binding in expected_bindings {
                assert!(
                    rendered.contains(binding),
                    "{binding:?} missing at width {width}; frame:\n{rendered}"
                );
            }
            assert!(!rendered.contains("Ask anything"));
            assert!(!rendered.contains("ctrl+p commands"));
            assert_eq!(
                scroll.viewport_height,
                height.saturating_sub(1 + dock.0.len() as u16) as usize
            );
            assert!(row_text(buffer, height - 1, width).contains("<leader>0"));
        }
    }
}
