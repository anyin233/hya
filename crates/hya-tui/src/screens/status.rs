use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::contracts::Rgba;
use crate::render::draw::{rgba_to_color, text_to_ratatui};
use crate::render::text::{Attrs, Line, Span, Text};
use crate::theme::ResolvedTheme;

const PANEL_WIDTH: u16 = 64;

pub struct StatusView<'a> {
    pub mcp: &'a [(String, String)],
    pub lsp: &'a [(String, String, String)],
    pub formatters: &'a [String],
    pub plugins: &'a [(String, Option<String>)],
}

pub fn draw(frame: &mut ratatui::Frame<'_>, view: &StatusView<'_>, theme: &ResolvedTheme) {
    let screen = frame.area();
    let bg = theme.background;
    let panel = rgba_to_color(theme.background_panel, bg);
    let lines = build_lines(view, theme);
    let width = PANEL_WIDTH.min(screen.width.saturating_sub(2));
    let body_height = (lines.len() as u16).saturating_add(2);
    let height = body_height.min(screen.height.saturating_sub(2)).max(3);
    let x = screen.x + screen.width.saturating_sub(width) / 2;
    let y = screen.y + screen.height.saturating_sub(height) / 4;
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(panel)), area);

    let content_area = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    };
    let body = text_to_ratatui(&Text(lines), bg);
    frame.render_widget(
        Paragraph::new(body).style(Style::default().fg(rgba_to_color(theme.text, bg)).bg(panel)),
        content_area,
    );
}

fn build_lines(view: &StatusView<'_>, theme: &ResolvedTheme) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(header_line(view, theme));
    lines.push(Line::default());
    append_mcp(&mut lines, view.mcp, theme);
    lines.push(Line::default());
    append_lsp(&mut lines, view.lsp, theme);
    lines.push(Line::default());
    append_formatters(&mut lines, view.formatters, theme);
    lines.push(Line::default());
    append_plugins(&mut lines, view.plugins, theme);
    lines
}

fn header_line(view: &StatusView<'_>, theme: &ResolvedTheme) -> Line {
    let _ = view;
    Line(vec![
        Span::styled(
            "Status",
            Some(theme.text),
            None,
            Attrs {
                bold: true,
                ..Attrs::default()
            },
        ),
        Span::plain("   "),
        Span::styled("esc", Some(theme.text_muted), None, Attrs::default()),
    ])
}

fn append_mcp(lines: &mut Vec<Line>, mcp: &[(String, String)], theme: &ResolvedTheme) {
    if mcp.is_empty() {
        lines.push(text_line("No MCP Servers", theme.text));
        return;
    }
    lines.push(text_line(format!("{} MCP Servers", mcp.len()), theme.text));
    for (name, status) in mcp {
        let bullet_color = mcp_color(status, theme);
        let detail = mcp_detail(name, status);
        lines.push(item_line(bullet_color, name, &detail, theme));
    }
}

fn append_lsp(lines: &mut Vec<Line>, lsp: &[(String, String, String)], theme: &ResolvedTheme) {
    if lsp.is_empty() {
        return;
    }
    lines.push(text_line(format!("{} LSP Servers", lsp.len()), theme.text));
    for (id, root, status) in lsp {
        let bullet_color = match status.as_str() {
            "connected" => theme.success,
            "error" => theme.error,
            _ => theme.text_muted,
        };
        lines.push(item_line(bullet_color, id, root, theme));
    }
}

fn append_formatters(lines: &mut Vec<Line>, formatters: &[String], theme: &ResolvedTheme) {
    if formatters.is_empty() {
        lines.push(text_line("No Formatters", theme.text));
        return;
    }
    lines.push(text_line(
        format!("{} Formatters", formatters.len()),
        theme.text,
    ));
    for name in formatters {
        lines.push(item_line(theme.success, name, "", theme));
    }
}

fn append_plugins(
    lines: &mut Vec<Line>,
    plugins: &[(String, Option<String>)],
    theme: &ResolvedTheme,
) {
    if plugins.is_empty() {
        lines.push(text_line("No Plugins", theme.text));
        return;
    }
    lines.push(text_line(format!("{} Plugins", plugins.len()), theme.text));
    for (name, version) in plugins {
        let detail = version
            .as_deref()
            .map(|v| format!("@{v}"))
            .unwrap_or_default();
        lines.push(item_line(theme.success, name, &detail, theme));
    }
}

fn mcp_color(status: &str, theme: &ResolvedTheme) -> Rgba {
    match status {
        "connected" => theme.success,
        "failed" | "needs_client_registration" => theme.error,
        "disabled" => theme.text_muted,
        "needs_auth" => theme.warning,
        _ => theme.text_muted,
    }
}

fn mcp_detail(name: &str, status: &str) -> String {
    match status {
        "connected" => "Connected".to_owned(),
        "disabled" => "Disabled in configuration".to_owned(),
        "needs_auth" => format!("Needs authentication (run: opencode mcp auth {name})"),
        other => other.to_owned(),
    }
}

fn text_line(content: impl Into<String>, fg: Rgba) -> Line {
    Line(vec![Span::styled(
        content.into(),
        Some(fg),
        None,
        Attrs::default(),
    )])
}

fn item_line(bullet: Rgba, primary: &str, detail: &str, theme: &ResolvedTheme) -> Line {
    let mut spans = vec![
        Span::styled("• ", Some(bullet), None, Attrs::default()),
        Span::styled(
            primary.to_owned(),
            Some(theme.text),
            None,
            Attrs {
                bold: true,
                ..Attrs::default()
            },
        ),
    ];
    if !detail.is_empty() {
        spans.push(Span::plain(" "));
        spans.push(Span::styled(
            detail.to_owned(),
            Some(theme.text_muted),
            None,
            Attrs::default(),
        ));
    }
    Line(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, resolve, Mode, DEFAULT_THEME};

    fn fixture_theme() -> ResolvedTheme {
        let json = builtin_theme(DEFAULT_THEME).unwrap().unwrap();
        resolve(&json, Mode::Dark).unwrap()
    }

    #[test]
    fn empty_view_uses_fallback_strings() {
        let theme = fixture_theme();
        let view = StatusView {
            mcp: &[],
            lsp: &[],
            formatters: &[],
            plugins: &[],
        };
        let lines = build_lines(&view, &theme);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.0
                    .iter()
                    .map(|span| span.text.clone())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|line| line.contains("No MCP Servers")));
        assert!(text.iter().any(|line| line.contains("No Formatters")));
        assert!(text.iter().any(|line| line.contains("No Plugins")));
        assert!(text.iter().all(|line| !line.contains("LSP Servers")));
    }

    #[test]
    fn populated_view_renders_counts_and_bullets() {
        let theme = fixture_theme();
        let view = StatusView {
            mcp: &[
                ("alpha".to_owned(), "connected".to_owned()),
                ("beta".to_owned(), "needs_auth".to_owned()),
            ],
            lsp: &[(
                "rust-analyzer".to_owned(),
                "/repo".to_owned(),
                "connected".to_owned(),
            )],
            formatters: &["prettier".to_owned()],
            plugins: &[("oh-my-openagent".to_owned(), Some("latest".to_owned()))],
        };
        let lines = build_lines(&view, &theme);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.0
                    .iter()
                    .map(|span| span.text.clone())
                    .collect::<String>()
            })
            .collect();
        assert!(text.iter().any(|line| line.contains("2 MCP Servers")));
        assert!(text.iter().any(|line| line.contains("1 LSP Servers")));
        assert!(text.iter().any(|line| line.contains("1 Formatters")));
        assert!(text.iter().any(|line| line.contains("1 Plugins")));
        assert!(text
            .iter()
            .any(|line| line.contains("opencode mcp auth beta")));
        assert!(text.iter().any(|line| line.contains("@latest")));
    }
}
