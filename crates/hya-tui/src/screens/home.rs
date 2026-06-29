use ratatui::widgets::{Block, Paragraph};

use crate::contracts::{
    Align, FlexSpec, Justify, NodeId, PromptDoc, Rect, RenderNode, Rgba, SizeHint,
};
use crate::render::{draw, flex};
use crate::theme::ResolvedTheme;

use super::prompt_box::{self, PromptBoxView};

const LOGO_ID: NodeId = NodeId(1);
const PROMPT_ID: NodeId = NodeId(2);
const PROMPT_MAX_WIDTH: u16 = 75;

fn prompt_width(area_width: u16) -> u16 {
    area_width.saturating_sub(4).clamp(20, PROMPT_MAX_WIDTH)
}

pub const PLACEHOLDERS: &[&str] = &[
    "Ask anything... \"Fix a TODO in the codebase\"",
    "Ask anything... \"What is the tech stack of this project?\"",
    "Ask anything... \"Fix broken tests\"",
];

#[must_use]
pub fn random_placeholder() -> &'static str {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos() as usize);
    PLACEHOLDERS[seed % PLACEHOLDERS.len()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeLayout {
    pub logo: Rect,
    pub prompt: Rect,
}

#[must_use]
pub fn compute_layout(area: Rect, prompt_height: u16) -> HomeLayout {
    let prompt_width = prompt_width(area.width);
    let root = RenderNode {
        id: None,
        flex: FlexSpec {
            justify: Justify::Center,
            align: Align::Center,
            gap: 1,
            ..FlexSpec::default()
        },
        children: vec![
            node(LOGO_ID, super::logo::LOGO_WIDTH, super::logo::LOGO_HEIGHT),
            node(PROMPT_ID, prompt_width, prompt_height),
        ],
    };
    let solved = flex::layout(&root, area);
    HomeLayout {
        logo: solved.get(LOGO_ID).unwrap_or_default(),
        prompt: solved.get(PROMPT_ID).unwrap_or_default(),
    }
}

pub struct HomeView<'a> {
    pub doc: &'a PromptDoc,
    pub agents: &'a [String],
    pub active_agent: Option<&'a str>,
    pub model_label: Option<&'a str>,
    pub provider_label: Option<&'a str>,
    pub tip: Option<&'a str>,
    pub placeholder: &'a str,
    pub mcp: Option<(usize, usize, bool)>,
    pub logo_elapsed: std::time::Duration,
    pub show_cursor: bool,
    pub yolo: bool,
}

pub fn draw(
    frame: &mut ratatui::Frame<'_>,
    view: &HomeView<'_>,
    theme: &ResolvedTheme,
) -> prompt_box::PromptHits {
    let HomeView {
        doc,
        agents,
        active_agent,
        model_label,
        provider_label,
        tip,
        placeholder,
        mcp,
        logo_elapsed,
        show_cursor,
        yolo,
    } = *view;
    let area = frame.area();
    let background = theme.background;
    frame.render_widget(
        Block::default().style(
            ratatui::style::Style::default().bg(draw::rgba_to_color(background, background)),
        ),
        area,
    );

    // Parity: home has no sidebar (unlike the session screen).
    let footer_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(2),
        width: area.width,
        height: 1,
    };
    let content_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(2),
    };

    let prompt_height = prompt_box::box_height(&doc.text, prompt_width(content_area.width));
    let layout = compute_layout(content_area, prompt_height);
    let logo = Paragraph::new(draw::text_to_ratatui(
        &super::logo::logo_text_at(theme, logo_elapsed),
        background,
    ))
    .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(logo, draw::rect_to_ratatui(layout.logo));

    let agent_label = active_agent.map(prompt_box::titlecase);
    let view = PromptBoxView {
        text: &doc.text,
        placeholder,
        agent_label: agent_label.as_deref(),
        agent_color: prompt_box::agent_color(theme, agents, active_agent),
        model_label,
        provider_label,
        shell_mode: false,
        working: false,
        spinner: "",
        agent_shortcut: "tab",
        palette_shortcut: "ctrl+p",
        cursor: doc.cursor,
        show_cursor,
        yolo,
    };
    let hits = prompt_box::draw(frame, layout.prompt, &view, theme);

    if let Some(tip) = tip {
        let tip_y = layout.prompt.y + layout.prompt.height + 2;
        if tip_y < footer_area.y.saturating_sub(1) {
            draw_tip(frame, layout.prompt, tip_y, tip, background, theme);
        }
    }

    draw_footer(frame, footer_area, mcp, background, theme);
    hits
}

fn draw_tip(
    frame: &mut ratatui::Frame<'_>,
    prompt: Rect,
    y: u16,
    tip: &str,
    background: Rgba,
    theme: &ResolvedTheme,
) {
    let mut spans = vec![ratatui::text::Span::styled(
        "\u{25cf} Tip ",
        ratatui::style::Style::default().fg(draw::rgba_to_color(theme.warning, background)),
    )];
    spans.extend(tip_spans(tip, background, theme));
    let area = Rect {
        x: prompt.x,
        y,
        width: prompt.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(ratatui::text::Line::from(spans)),
        draw::rect_to_ratatui(area),
    );
}

fn tip_spans(
    tip: &str,
    background: Rgba,
    theme: &ResolvedTheme,
) -> Vec<ratatui::text::Span<'static>> {
    let muted =
        ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text_muted, background));
    let strong = ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background));
    let mut spans = Vec::new();
    let mut rest = tip;
    while let Some(start) = rest.find("{highlight}") {
        if start > 0 {
            spans.push(ratatui::text::Span::styled(rest[..start].to_owned(), muted));
        }
        rest = &rest[start + "{highlight}".len()..];
        match rest.find("{/highlight}") {
            Some(end) => {
                spans.push(ratatui::text::Span::styled(rest[..end].to_owned(), strong));
                rest = &rest[end + "{/highlight}".len()..];
            }
            None => break,
        }
    }
    if !rest.is_empty() {
        spans.push(ratatui::text::Span::styled(rest.to_owned(), muted));
    }
    spans
}

pub const TIPS: &[&str] = &[
    "Type {highlight}@{/highlight} followed by a filename to fuzzy search and attach files",
    "Start a message with {highlight}!{/highlight} to run shell commands directly (e.g., {highlight}!ls -la{/highlight})",
    "Use {highlight}/undo{/highlight} to revert the last message and file changes",
    "Use {highlight}/redo{/highlight} to restore previously undone messages and file changes",
    "Drag and drop images or PDFs into the terminal to add them as context",
    "Run {highlight}/init{/highlight} to auto-generate project rules based on your codebase",
    "Run {highlight}/compact{/highlight} to summarize long sessions near context limits",
];

#[must_use]
pub fn random_tip() -> &'static str {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as usize);
    TIPS[seed % TIPS.len()]
}

fn draw_footer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    mcp: Option<(usize, usize, bool)>,
    background: Rgba,
    theme: &ResolvedTheme,
) {
    let muted =
        ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text_muted, background));
    let directory = abbreviate_home(&current_directory());
    let row = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    let mut left = vec![ratatui::text::Span::styled(directory, muted)];
    if let Some((connected, _total, has_error)) = mcp {
        let dot_color = if has_error {
            theme.error
        } else if connected > 0 {
            theme.success
        } else {
            theme.text_muted
        };
        left.push(ratatui::text::Span::raw("  "));
        left.push(ratatui::text::Span::styled(
            "\u{2299} ",
            ratatui::style::Style::default().fg(draw::rgba_to_color(dot_color, background)),
        ));
        left.push(ratatui::text::Span::styled(
            format!("{connected} MCP"),
            ratatui::style::Style::default().fg(draw::rgba_to_color(theme.text, background)),
        ));
        left.push(ratatui::text::Span::styled(" /status", muted));
    }
    let version = env!("CARGO_PKG_VERSION");
    let version_width = (version.len() as u16 + 1).min(row.width);
    let left_row = Rect {
        x: row.x,
        y: row.y,
        width: row.width.saturating_sub(version_width),
        height: 1,
    };
    let version_row = Rect {
        x: row.x + row.width.saturating_sub(version_width),
        y: row.y,
        width: version_width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(ratatui::text::Line::from(left)).alignment(ratatui::layout::Alignment::Left),
        draw::rect_to_ratatui(left_row),
    );
    frame.render_widget(
        Paragraph::new(version)
            .style(muted)
            .alignment(ratatui::layout::Alignment::Right),
        draw::rect_to_ratatui(version_row),
    );
}

fn current_directory() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn abbreviate_home(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_owned(),
    }
}

fn node(id: NodeId, width: u16, height: u16) -> RenderNode {
    RenderNode {
        id: Some(id),
        flex: FlexSpec {
            width: SizeHint::Cells(width),
            height: SizeHint::Cells(height),
            ..FlexSpec::default()
        },
        children: Vec::new(),
    }
}

#[must_use]
pub fn default_background(theme: &ResolvedTheme) -> Rgba {
    theme.background
}
