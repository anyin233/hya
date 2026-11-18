//! Integration tests for `hya-tool`: capping tool output without losing it.
//!
//! The cap decides how much of a result reaches the transcript. Spilling decides
//! whether the rest is still reachable afterwards. These assert the second part,
//! which is the difference between a truncated result and a lost one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::handle::ArtifactStore;
use hya_tool::{MAX_TOOL_OUTPUT_CHARS, ToolResultPolicy, cap_tool_output_spilling};
use serde_json::{Value, json};

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("hya-spill-{nanos}-{}-{id}", std::process::id()))
}

/// The artifact id named by a truncation notice.
fn handle_in(notice: &str) -> &str {
    let start = notice.find("artifact://").expect("notice names a handle") + "artifact://".len();
    let rest = &notice[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// An oversized result is bounded *and* recoverable: the notice carries the
/// address of the whole thing.
#[test]
fn oversized_default_output_spills_and_names_its_handle() {
    // Given
    let root = tempdir();
    let store = ArtifactStore::new(&root);
    let body = format!(
        "HEAD_LINE\n{}\nTAIL_LINE",
        "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2)
    );

    // When
    let capped = cap_tool_output_spilling(
        Value::String(body.clone()),
        ToolResultPolicy::Default,
        &store,
        "bash",
    );

    // Then
    let notice = capped.as_str().expect("an oversized result becomes text");
    assert!(notice.contains("truncated"), "{notice}");
    let stored = std::fs::read_to_string(root.join(handle_in(notice))).unwrap();
    assert_eq!(stored, body, "the spilled artifact is the complete output");
    // Head and tail both survive; the marker between them names the omitted
    // size and the same artifact.
    assert!(notice.contains("\nHEAD_LINE\n"), "{notice}");
    assert!(notice.ends_with("\nTAIL_LINE"), "{notice}");
    let marker = notice
        .lines()
        .find(|line| line.starts_with("[… ") && line.contains("chars omitted"))
        .expect("a marker between head and tail");
    assert!(
        marker.contains(&format!("artifact://{}", handle_in(notice))),
        "{marker}"
    );
    assert!(
        notice.chars().count() <= MAX_TOOL_OUTPUT_CHARS + 200,
        "{notice}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A result that fits is returned with its shape intact, and costs no artifact.
#[test]
fn output_under_the_cap_is_untouched_and_writes_nothing() {
    // Given
    let root = tempdir();
    let store = ArtifactStore::new(&root);
    let original = json!({ "rows": [1, 2, 3] });

    // When
    let capped =
        cap_tool_output_spilling(original.clone(), ToolResultPolicy::Default, &store, "grep");

    // Then
    assert_eq!(capped, original);
    assert!(
        !root.exists(),
        "a result that fits must not create an artifact directory"
    );
}

/// A coding envelope already describes a file the model can re-read by path, so
/// spilling it would store a second copy of something already addressable.
#[test]
fn coding_envelopes_do_not_spill() {
    // Given
    let root = tempdir();
    let store = ArtifactStore::new(&root);
    let envelope = json!({
        "title": "src/main.rs",
        "output": "y".repeat(MAX_TOOL_OUTPUT_CHARS * 2),
        "metadata": {},
    });

    // When
    let capped = cap_tool_output_spilling(envelope, ToolResultPolicy::Coding, &store, "read");

    // Then
    assert!(
        capped.get("title").is_some(),
        "the coding envelope must survive: {capped}"
    );
    assert!(!root.exists(), "no artifact is written for a coding result");
}
