#![allow(dead_code, clippy::unwrap_used)]

mod render_support;

use yaca_proto::Role;
use yaca_tui::AppState;

use render_support::{buffer_text, render, render_buffer, with_text_message};

#[test]
fn selected_block_scrolls_into_view_when_above_the_current_viewport() {
    // Given: a long transcript is anchored near the latest message.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(
        &mut state,
        1,
        Role::User,
        "top selected prompt that should be visible",
    );
    for idx in 0..16 {
        with_text_message(
            &mut state,
            10 + (idx * 10),
            Role::Assistant,
            &format!("filler assistant block {idx}"),
        );
    }

    // When: rendering starts from the bottom of the transcript.
    let text = render(&mut state, 80, 18);

    // Then: the selected block is pulled into the viewport for block-level reading/actions.
    assert!(
        text.contains("top selected prompt that should be visible"),
        "selected message should be visible after render:\n{text}"
    );
    assert!(
        state.scroll_back > 0,
        "selected block visibility should move scrollback away from the bottom"
    );
}

#[test]
fn end_scroll_can_return_to_latest_after_selected_block_was_visible() {
    // Given: a selected older block has pulled the viewport away from the latest message.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::User, "old selected prompt");
    for idx in 0..16 {
        with_text_message(
            &mut state,
            10 + (idx * 10),
            Role::Assistant,
            &format!("filler assistant block {idx}"),
        );
    }
    let selected_view = render(&mut state, 80, 18);
    assert!(selected_view.contains("old selected prompt"));

    // When: the controller's End key behavior returns scrollback to the bottom.
    state.scroll_back = 0;
    let latest_view = render(&mut state, 80, 18);

    // Then: selection no longer traps the viewport away from the latest content.
    assert!(
        latest_view.contains("filler assistant block 15"),
        "End-style bottom scroll should keep the latest block visible:\n{latest_view}"
    );
}

#[test]
fn selected_tall_block_keeps_its_start_visible_in_tiny_viewports() {
    // Given: the selected block is taller than the transcript viewport.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(
        &mut state,
        1,
        Role::User,
        "selected tall block first line\nsecond line\nthird line\nfourth line",
    );
    for idx in 0..8 {
        with_text_message(
            &mut state,
            10 + (idx * 10),
            Role::Assistant,
            &format!("filler assistant block {idx}"),
        );
    }

    // When: only a tiny transcript viewport is available.
    let text = render(&mut state, 80, 10);

    // Then: newly selected tall blocks show their start, not only their tail.
    assert!(
        text.contains("selected tall block first line"),
        "tall selected block should expose its start:\n{text}"
    );
}

#[test]
fn selected_cjk_block_scrolls_into_view_on_narrow_widths() {
    // Given: a wide-character selected block needs ratatui wrapping.
    let mut state = AppState {
        selected_message: Some(0),
        ..AppState::default()
    };
    with_text_message(
        &mut state,
        1,
        Role::User,
        "请检查这个界面是否保持无边框设计并正确处理宽字符换行",
    );
    for idx in 0..14 {
        with_text_message(
            &mut state,
            10 + (idx * 10),
            Role::Assistant,
            &format!("filler assistant block {idx}"),
        );
    }

    // When: the narrow transcript renders from the bottom.
    let buffer = render_buffer(&mut state, 50, 18);
    let text = buffer_text(&buffer, 50, 18);

    // Then: the selected CJK block remains reachable without overflowing the terminal.
    assert!(
        text.lines()
            .any(|line| line.contains('请') && line.contains('界')),
        "selected CJK block should render visible wide glyphs:\n{text}"
    );
}
