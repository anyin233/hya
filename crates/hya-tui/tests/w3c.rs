use std::time::{Duration, Instant};

use hya_tui::contracts::{Rect, Rgba};
use hya_tui::render::markdown;
use hya_tui::render::overlay::centered_rect;
use hya_tui::render::scroll::ScrollState;
use hya_tui::render::text::{Attrs, Line, Span, Text};
use hya_tui::theme::{builtin_themes, resolve, Mode, ResolvedTheme, DEFAULT_THEME};
use hya_tui::widgets::dialog_select::{DialogSelect, DialogSelectItem};
use hya_tui::widgets::spinner;
use hya_tui::widgets::toast::{Toast, ToastState, ToastVariant, DEFAULT_TIMEOUT};

fn default_theme() -> ResolvedTheme {
    let themes = builtin_themes().expect("builtin themes parse");
    resolve(
        themes.get(DEFAULT_THEME).expect("default theme exists"),
        Mode::Dark,
    )
    .expect("dark theme resolves")
}

fn plain_text(text: &Text) -> Vec<String> {
    text.0
        .iter()
        .map(|line| line.0.iter().map(|span| span.text.as_str()).collect())
        .collect()
}

#[test]
fn line_wrap_when_long_words_and_cjk_width_exceed_target() {
    let wrapped = Line(vec![Span::plain("hello world from rust")]).wrap(10);
    assert_eq!(plain_text(&Text(wrapped)), ["hello", "world from", "rust"]);

    let cjk = Text(vec![Line(vec![Span::plain("你好abc")])]).wrap(4);
    assert_eq!(plain_text(&cjk), ["你好", "abc"]);

    let explicit = Text(vec![Line(vec![Span::plain("a\nb\n")])]).wrap(10);
    assert_eq!(plain_text(&explicit), ["a", "b", ""]);
}

#[test]
fn markdown_parse_when_heading_bold_inline_code_and_fence_are_present() {
    let theme = default_theme();
    let rendered = markdown::parse(
        "# Title\n\nThis is **bold** and `code`.\n\n> [ref](https://example.com)\n\n- item\n\n```rust\nfn main() {}\n```",
        &theme,
    );

    assert_eq!(rendered.0[0].0[0].text, "Title");
    assert_eq!(rendered.0[0].0[0].fg, Some(theme.markdown_heading));
    assert!(rendered.0[0].0[0].attrs.bold);

    let paragraph = &rendered.0[1].0;
    let bold = paragraph
        .iter()
        .find(|span| span.text == "bold")
        .expect("bold span exists");
    assert_eq!(bold.fg, Some(theme.markdown_strong));
    assert!(bold.attrs.bold);

    let code = paragraph
        .iter()
        .find(|span| span.text == "code")
        .expect("inline code span exists");
    assert_eq!(code.fg, Some(theme.markdown_code));
    assert_eq!(code.bg, Some(theme.markdown_code_block));

    let link = rendered
        .0
        .iter()
        .flat_map(|line| &line.0)
        .find(|span| span.text == "ref")
        .expect("link text span exists");
    assert_eq!(link.fg, Some(theme.markdown_link_text));
    assert!(link.attrs.underline);

    let quote = rendered
        .0
        .iter()
        .flat_map(|line| &line.0)
        .find(|span| span.text == "> ")
        .expect("blockquote prefix exists");
    assert_eq!(quote.fg, Some(theme.markdown_block_quote));

    let bullet = rendered
        .0
        .iter()
        .flat_map(|line| &line.0)
        .find(|span| span.text == "- ")
        .expect("list bullet span exists");
    assert_eq!(bullet.fg, Some(theme.markdown_list_item));

    let code_line = rendered
        .0
        .iter()
        .find(|line| line.0.iter().any(|span| span.text.contains("fn")))
        .expect("fenced rust code highlighted");
    assert!(code_line
        .0
        .iter()
        .any(|span| span.text == "fn" && span.fg == Some(theme.syntax_keyword)));
}

#[test]
fn scroll_state_when_content_changes_clamps_and_sticks_to_bottom() {
    let mut state = ScrollState::new(100, 20);
    state.to_bottom();
    assert_eq!(state.offset, 80);

    state.sticky_bottom(100, 140);
    assert_eq!(state.offset, 120);

    state.scroll_by(-200);
    assert_eq!(state.offset, 0);

    state.page_down();
    assert_eq!(state.offset, 20);
    state.half_page_up();
    assert_eq!(state.offset, 10);

    state.set_viewport_height(200);
    assert_eq!(state.offset, 0);
}

#[test]
fn centered_rect_when_modal_is_smaller_than_area_places_it_in_the_middle() {
    let area = Rect {
        x: 10,
        y: 5,
        width: 100,
        height: 40,
    };

    assert_eq!(
        centered_rect(30, 10, area),
        Rect {
            x: 45,
            y: 20,
            width: 30,
            height: 10,
        }
    );
}

#[test]
fn dialog_select_when_filtering_and_navigating_uses_fuzzy_matches() {
    let mut select = DialogSelect::new(vec![
        DialogSelectItem::new("Open File", 1).with_category("Command"),
        DialogSelectItem::new("Close Tab", 2).with_category("Command"),
        DialogSelectItem::new("Banana", 3).with_category("Fruit"),
    ]);

    select.set_filter("opn");
    assert_eq!(select.filtered_items()[0].title, "Open File");
    assert_eq!(select.select(), Some(&1));

    select.set_filter("command");
    assert_eq!(select.filtered_items().len(), 2);
    select.move_up();
    assert_eq!(select.select(), Some(&2));
    select.move_down();
    assert_eq!(select.select(), Some(&1));
}

#[test]
fn toast_when_timeout_elapses_expires_current_message() {
    let now = Instant::now();
    let toast = Toast::new("Saved", ToastVariant::Success, now);
    let mut state = ToastState::default();

    state.show(toast);
    assert_eq!(
        state.current().map(|toast| toast.message.as_str()),
        Some("Saved")
    );

    state.expire(now + DEFAULT_TIMEOUT - Duration::from_millis(1));
    assert!(state.current().is_some());
    state.expire(now + DEFAULT_TIMEOUT);
    assert!(state.current().is_none());
}

#[test]
fn spinner_frame_when_elapsed_advances_cycles_exact_origin_frames() {
    assert_eq!(spinner::frame(Duration::from_millis(0)), "⠋");
    assert_eq!(spinner::frame(Duration::from_millis(80)), "⠙");
    assert_eq!(spinner::frame(Duration::from_millis(800)), "⠋");
    assert_eq!(spinner::ellipsis_fallback("Loading"), "⋯ Loading");
}

#[test]
fn span_plain_when_constructed_has_no_color_or_attrs() {
    assert_eq!(
        Span::plain("text"),
        Span {
            text: "text".to_owned(),
            fg: None,
            bg: None,
            attrs: Attrs::default(),
        }
    );
    assert_ne!(Rgba::TRANSPARENT, Rgba::rgb(0, 0, 0));
}
