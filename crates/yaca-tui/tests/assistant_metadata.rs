#![allow(clippy::unwrap_used)]

#[allow(dead_code)]
mod render_support;

use yaca_proto::Role;
use yaca_proto::{Envelope, Event, EventSeq, FinishReason, MessageId, PartId, SessionId};
use yaca_tui::AppState;

use render_support::{find_rendered_text, render_buffer, with_text_message};

fn row_text(buffer: &ratatui::buffer::Buffer, width: u16, y: u16) -> String {
    let mut row = String::new();
    for x in 0..width {
        row.push_str(buffer[(x, y)].symbol());
    }
    row
}

fn assistant_metadata_rows(
    buffer: &ratatui::buffer::Buffer,
    width: u16,
    height: u16,
    needle: &str,
) -> Vec<String> {
    let mut rows = Vec::new();
    for y in 0..height {
        let row = row_text(buffer, width, y);
        if row.starts_with("     ▣ ") && row.contains(needle) {
            rows.push(row);
        }
    }
    rows
}

#[test]
fn assistant_block_renders_message_metadata_footer() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "metadata ready");

    let buffer = render_buffer(&mut state, 80, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 80, 16, "metadata ready").unwrap();
    let metadata_row = row_text(&buffer, 80, text_y + 1);

    assert!(
        metadata_row.contains("Sisyphus · kimi-k2 · completed"),
        "assistant metadata footer should sit under the message text, got {metadata_row:?}"
    );
    assert!(
        metadata_row.contains("▣ Sisyphus"),
        "assistant metadata footer should use the OpenCode turn marker, got {metadata_row:?}"
    );
}

#[test]
fn assistant_metadata_footer_includes_active_agent_role() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        team: vec![("sisyphus".to_string(), "ultraworker retry".to_string())],
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "metadata with role");

    let buffer = render_buffer(&mut state, 100, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 100, 16, "metadata with role").unwrap();
    let metadata_row = row_text(&buffer, 100, text_y + 1);

    assert!(
        metadata_row.contains("Sisyphus - Ultraworker Retry · kimi-k2 · completed"),
        "assistant metadata should include the active agent role, got {metadata_row:?}"
    );
}

#[test]
fn assistant_metadata_footer_includes_provider_label() {
    // Given: the active model has the same provider label shown in the composer.
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        model_provider_label: Some("GLM/Kimi".to_string()),
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "metadata with provider");

    // When: the assistant block renders its OpenCode-style metadata footer.
    let buffer = render_buffer(&mut state, 100, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 100, 16, "metadata with provider").unwrap();
    let metadata_row = row_text(&buffer, 100, text_y + 1);

    // Then: assistant block identity matches composer identity by keeping the provider label.
    assert!(
        metadata_row.contains("Sisyphus · kimi-k2 GLM/Kimi · completed"),
        "assistant metadata should include the provider label, got {metadata_row:?}"
    );
}

#[test]
fn assistant_metadata_footer_uses_finished_turn_duration() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        ..AppState::default()
    };
    with_timed_assistant_message(&mut state, 1_000, 82_000, "metadata with duration");

    let buffer = render_buffer(&mut state, 100, 16);
    let (_x, text_y) = find_rendered_text(&buffer, 100, 16, "metadata with duration").unwrap();
    let metadata_row = row_text(&buffer, 100, text_y + 1);

    assert!(
        metadata_row.contains("▣ Sisyphus · kimi-k2 · 1m 21s"),
        "assistant metadata should use the finished turn duration, got {metadata_row:?}"
    );
    assert!(
        !metadata_row.contains("completed"),
        "finished assistant metadata should not fall back to status text when duration exists"
    );
}

#[test]
fn running_assistant_block_omits_synthetic_streaming_status() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "first response");
    with_text_message(&mut state, 10, Role::Assistant, "second response");

    let buffer = render_buffer(&mut state, 100, 20);
    let metadata_rows = assistant_metadata_rows(&buffer, 100, 20, "Sisyphus · kimi-k2");

    assert_eq!(
        metadata_rows.len(),
        2,
        "expected one metadata footer per assistant block, got {metadata_rows:?}"
    );
    assert!(
        metadata_rows[0].contains("Sisyphus · kimi-k2 · completed"),
        "older assistant block should remain completed, got {:?}",
        metadata_rows[0]
    );
    assert!(
        metadata_rows[1].contains("Sisyphus · kimi-k2"),
        "latest assistant block should keep identity visible, got {:?}",
        metadata_rows[1]
    );
    assert!(
        !metadata_rows[1].contains("streaming"),
        "OpenCode running assistant footer omits synthetic streaming text, got {:?}",
        metadata_rows[1]
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

#[test]
fn prior_assistant_block_stays_completed_while_new_user_turn_waits() {
    let mut state = AppState {
        agent: "sisyphus".to_string(),
        model: "kimi-k2".to_string(),
        running: true,
        ..AppState::default()
    };
    with_text_message(&mut state, 1, Role::Assistant, "previous response");
    with_text_message(&mut state, 10, Role::User, "new prompt");

    let buffer = render_buffer(&mut state, 100, 20);
    let metadata_rows = assistant_metadata_rows(&buffer, 100, 20, "Sisyphus · kimi-k2 ·");

    assert_eq!(
        metadata_rows.len(),
        1,
        "expected only the existing assistant footer, got {metadata_rows:?}"
    );
    assert!(
        metadata_rows[0].contains("Sisyphus · kimi-k2 · completed"),
        "previous assistant block should remain completed while waiting, got {:?}",
        metadata_rows[0]
    );
}
