#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use ratatui::buffer::Buffer;
use render_support::render_buffer;
use yaca_proto::{Envelope, Event, EventSeq, FinishReason, MessageId, PartId, Role, SessionId};
use yaca_tui::AppState;

fn row_text(buffer: &Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn find_row(buffer: &Buffer, width: u16, height: u16, needle: &str) -> String {
    (0..height)
        .map(|y| row_text(buffer, width, y))
        .find(|row| row.contains(needle))
        .unwrap()
}

#[test]
fn runtime_status_uses_latest_finished_assistant_duration_when_idle() {
    // Given: the latest assistant turn has an OpenCode-style elapsed duration.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        ..AppState::default()
    };
    with_timed_assistant_message(&mut state, 1_000, 82_000, "runtime duration");

    // When: the grounded runtime strip renders above the composer while idle.
    let buffer = render_buffer(&mut state, 120, 16);
    let status_row = find_row(&buffer, 120, 16, "1m 21s");

    // Then: it mirrors OpenCode's elapsed-turn metadata instead of plain idle.
    assert!(
        status_row.contains("sisyphus · kimi-k2 · 1m 21s"),
        "runtime strip should show the latest assistant duration, got {status_row:?}"
    );
    assert!(
        !status_row.contains("idle"),
        "runtime strip should not fall back to idle when a turn duration exists"
    );
}

#[test]
fn runtime_status_omits_idle_placeholder_when_no_duration_exists() {
    // Given: an idle shell before any assistant duration exists.
    let mut state = AppState {
        agent: "build".to_string(),
        model: "kimi-k2".to_string(),
        ..AppState::default()
    };

    // When: the grounded runtime strip renders above the composer.
    let buffer = render_buffer(&mut state, 100, 16);
    let status_row = find_row(&buffer, 100, 16, "build · kimi-k2");

    // Then: it mirrors OpenCode by omitting idle filler metadata.
    assert!(
        !status_row.contains("idle"),
        "runtime strip should not show idle filler, got {status_row:?}"
    );
    assert!(
        !status_row.contains("kimi-k2 ·"),
        "runtime strip should not leave a dangling separator, got {status_row:?}"
    );
}

fn with_timed_assistant_message(
    state: &mut AppState,
    started_ms: i64,
    finished_ms: i64,
    text: &str,
) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    state.apply(&timed_env(
        1,
        started_ms,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&timed_env(
        2,
        started_ms,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    state.apply(&timed_env(
        3,
        started_ms,
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
    ));
    state.apply(&timed_env(
        4,
        finished_ms,
        Event::MessageFinished {
            session,
            message,
            finish: FinishReason::Stop,
        },
    ));
}

fn timed_env(seq: u64, ts_millis: i64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis,
        event,
    }
}
