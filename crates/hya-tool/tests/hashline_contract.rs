//! Public `ToolRegistry` tracer contracts for the native hashline coding tools.
//!
//! The golden anchors and strict edit envelope in this suite are pinned to
//! `pi-hashline-edit` 0.8.3 (npm `gitHead`
//! `ba7db9943d0f58499b24c1f6bd64722580f772a5`, tarball SHA-1
//! `8985f24c3493be375cc225a5522ed54de8daabc9`). The Rust implementation must
//! preserve the MIT-licensed package's observable `LINE#HASH:content` output
//! and `{ path, edits }` request shape without exposing its private hashline
//! machinery as a test seam.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::SessionId;
use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Build an allow rule for a tool resource in the test permission plane.
fn allow(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Allow)
}

/// Create an isolated temporary workdir without relying on an external crate.
fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-hashline-contract-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Construct a context without a session identity for local tool contracts.
fn ctx_with(workdir: PathBuf) -> ToolCtx {
    ctx_with_session(workdir, None)
}

/// Construct the permission- and plane-backed context for an optional session.
fn ctx_with_session(workdir: PathBuf, session: Option<SessionId>) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Read, "*"),
        allow(Action::Edit, "*"),
    ]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

/// Extract a one-based `LINE#HASH` anchor from public Read output.
fn anchor_for_line(read_result: &Value, line: usize) -> String {
    let output = read_result["output"]
        .as_str()
        .expect("Read result must expose string output");
    output
        .lines()
        .find_map(|row| {
            let row = row.trim_start();
            let (prefix, _) = row.split_once(':')?;
            let (line_text, hash) = prefix.split_once('#')?;
            let parsed_line = line_text.parse::<usize>().ok()?;
            (parsed_line == line).then(|| format!("{parsed_line}#{hash}"))
        })
        .unwrap_or_else(|| panic!("Read output did not contain anchor for line {line}"))
}

/// Assert that a failed Read range uses the stable typed input error.
fn assert_bad_read(error: hya_tool::ToolError) {
    match error {
        hya_tool::ToolError::Input(message) => {
            assert!(
                message.starts_with("[E_BAD_READ]"),
                "out-of-range Read must retain the stable code: {message:?}"
            );
        }
        other => panic!("out-of-range Read must be a typed input error: {other:?}"),
    }
}

#[tokio::test]
async fn read_emits_pinned_hashline_anchors_through_tool_registry() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let tool = registry.get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();

    // Then
    // Provenance: these literal vectors are the pinned pi-hashline-edit 0.8.3
    // two-character contextual XXH32 anchors, not values recomputed by the test.
    for row in ["1#KT:alpha", "2#JB:beta", "3#KJ:gamma", "4#PX:delta"] {
        assert!(
            output.contains(row),
            "missing pinned hashline row {row:?} in {output:?}"
        );
    }
}

#[tokio::test]
async fn read_does_not_render_a_synthetic_row_for_a_final_newline() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();

    // Then
    assert!(output.contains("4#PX:delta"));
    assert!(
        !output.lines().any(|line| line.starts_with("5#")),
        "a terminal newline must not create a fifth visible row: {output:?}"
    );
}

#[tokio::test]
async fn read_returns_clean_paged_content_and_display_metadata() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, "alpha\nbeta\ngamma\ndelta\nepsilon\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(
            &ctx,
            json!({ "path": "notes.txt", "offset": 2, "limit": 2 }),
        )
        .await
        .unwrap();

    // Then
    let display_path = target.to_string_lossy().to_string();
    assert_eq!(out["title"], "notes.txt");
    assert_eq!(out["content"], "beta\ngamma");
    assert_eq!(out["metadata"]["preview"], "beta\ngamma");
    assert_eq!(out["metadata"]["loaded"], json!([]));
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["type"], "file");
    assert_eq!(out["metadata"]["display"]["path"], display_path);
    assert_eq!(out["metadata"]["display"]["text"], "beta\ngamma");
    assert_eq!(out["metadata"]["display"]["lineStart"], 2);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 3);
    assert_eq!(out["metadata"]["display"]["totalLines"], 5);
    assert_eq!(out["metadata"]["display"]["truncated"], true);
    assert_eq!(out["metadata"]["nextOffset"], 4);

    let output = out["output"].as_str().unwrap();
    assert!(output.lines().any(|row| row.ends_with(":beta")));
    assert!(output.lines().any(|row| row.ends_with(":gamma")));
    assert!(!output.lines().any(|row| row.ends_with(":alpha")));
}

#[tokio::test]
async fn read_paging_emits_bounded_continuation_and_metadata() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("paged.txt");
    let source = (1..=12)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(&target, source).await.unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(
            &ctx,
            json!({ "path": "paged.txt", "offset": 4, "limit": 3 }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["content"], "line-4\nline-5\nline-6");
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["lineStart"], 4);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 6);
    assert_eq!(out["metadata"]["display"]["totalLines"], 12);
    assert_eq!(out["metadata"]["display"]["truncated"], true);
    assert!(out["output"].as_str().unwrap().contains("Use offset=7"));
}

#[tokio::test]
async fn read_byte_cap_bounds_hashline_output_without_tail_content() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("large.txt");
    let filler = "x".repeat(1000);
    let mut lines = (0..60).map(|_| filler.clone()).collect::<Vec<_>>();
    let tail = "TAIL_CONTENT_MUST_NOT_BE_INCLUDED";
    lines.push(tail.to_string());
    tokio::fs::write(&target, lines.join("\n")).await.unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "large.txt", "limit": 2000 }))
        .await
        .unwrap();

    // Then
    let output = out["output"].as_str().unwrap();
    let content = out["content"].as_str().unwrap();
    assert!(
        output.len() <= 50 * 1024,
        "Read output exceeded its byte cap"
    );
    assert!(
        content.len() <= 50 * 1024,
        "Read content exceeded its byte cap"
    );
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["totalLines"], 61);
    assert!(out["metadata"]["display"]["lineEnd"].as_u64().unwrap() < 61);
    assert!(output.contains("Showing lines"));
    assert!(!output.contains(tail));
    assert!(!content.contains(tail));
}

#[tokio::test]
async fn read_first_line_oversize_diagnostic_does_not_leak_content() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("oversize.txt");
    let secret = "OVERSIZE_LINE_SECRET";
    let line = format!("{secret}{}", "x".repeat(50 * 1024));
    tokio::fs::write(&target, line).await.unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "oversize.txt" }))
        .await
        .unwrap();

    // Then
    let output = out["output"].as_str().unwrap();
    assert!(output.len() <= 50 * 1024);
    assert!(output.contains("exceeds 51200 bytes"));
    assert!(!output.contains(secret));
    assert_eq!(out["content"], "");
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["lineStart"], 1);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 1);
    assert_eq!(out["metadata"]["display"]["totalLines"], 1);
}

#[tokio::test]
async fn read_raw_true_omits_hashline_anchors() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let tool = registry.get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let anchored = tool
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let raw = tool
        .execute(&ctx, json!({ "path": "notes.txt", "raw": true }))
        .await
        .unwrap();

    // Then
    let anchored_output = anchored["output"].as_str().unwrap();
    let raw_output = raw["output"].as_str().unwrap();
    assert!(anchored_output.contains("1#KT:alpha"));
    assert!(!raw_output.contains("1#KT:alpha"));
    assert_eq!(raw_output, "alpha\nbeta\ngamma\ndelta");
    assert_ne!(
        anchored_output, raw_output,
        "raw mode must switch the model-visible representation"
    );
    assert_eq!(raw["content"], "alpha\nbeta\ngamma\ndelta");
}

#[tokio::test]
async fn read_to_edit_uses_fresh_public_anchor_chain() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, "alpha\nbeta\ngamma\ndelta\nepsilon")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let read_before = read
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let beta_anchor = anchor_for_line(&read_before, 2);

    // When
    let edit_result = edit
        .execute(
            &ctx,
            json!({
                "path": "notes.txt",
                "edits": [{
                    "op": "replace",
                    "pos": beta_anchor,
                    "lines": ["BETA"]
                }]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "alpha\nBETA\ngamma\ndelta\nepsilon"
    );
    assert_eq!(edit_result["metadata"]["classification"], "applied");
    assert_eq!(edit_result["metadata"]["warnings"], json!([]));

    let read_after = read
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let fresh_beta_anchor = anchor_for_line(&read_after, 2);
    assert_ne!(fresh_beta_anchor, beta_anchor);
    assert!(
        edit_result["output"]
            .as_str()
            .unwrap()
            .contains(&fresh_beta_anchor),
        "successful edit must expose anchors for its final bytes"
    );
}

#[tokio::test]
async fn non_raw_read_captures_newest_snapshot_for_far_stale_recovery() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("history.txt");
    let base_lines = (1..=12)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>();
    tokio::fs::write(&target, base_lines.join("\n"))
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let first_read = read
        .execute(&ctx, json!({ "path": "history.txt" }))
        .await
        .unwrap();
    let stale_line_three = anchor_for_line(&first_read, 3);
    let fresh_line_ten = anchor_for_line(&first_read, 10);

    // An external line-2 change stales line 3's contextual anchor. The real
    // edit is far away at line 10, so its context-three merge remains exact.
    let mut external_lines = base_lines;
    external_lines[1] = "EXTERNAL_FAR_CHANGE_SECRET".to_string();
    tokio::fs::write(&target, external_lines.join("\n"))
        .await
        .unwrap();
    let _newest_read = read
        .execute(&ctx, json!({ "path": "history.txt" }))
        .await
        .unwrap();

    // When
    let edit_result = edit
        .execute(
            &ctx,
            json!({
                "path": "history.txt",
                "edits": [
                    {
                        "op": "replace",
                        "pos": stale_line_three,
                        "lines": ["line-3"]
                    },
                    {
                        "op": "replace",
                        "pos": fresh_line_ten,
                        "lines": ["LINE-10-EDITED"]
                    }
                ]
            }),
        )
        .await
        .unwrap();

    // Then
    let mut expected = external_lines;
    expected[9] = "LINE-10-EDITED".to_string();
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        expected.join("\n")
    );
    assert_eq!(edit_result["metadata"]["classification"], "applied");
    assert!(
        edit_result["output"]
            .as_str()
            .unwrap()
            .contains("Recovered stale anchors")
    );
}

#[tokio::test]
async fn raw_read_does_not_seed_snapshot_for_stale_edit() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("raw.txt");
    let original = "alpha\nbeta\ngamma\ndelta";
    tokio::fs::write(&target, original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let raw = read
        .execute(&ctx, json!({ "path": "raw.txt", "raw": true }))
        .await
        .unwrap();
    assert_eq!(raw["output"], original);
    let external = "alpha\nbeta\nRAW_EXTERNAL_SECRET\ndelta";
    tokio::fs::write(&target, external).await.unwrap();

    // When
    // Provenance: 2#JB is a pinned golden anchor for the original four-line
    // fixture above. Raw Read output intentionally cannot supply an anchor.
    let error = edit
        .execute(
            &ctx,
            json!({
                "path": "raw.txt",
                "edits": [{
                    "op": "replace",
                    "pos": "2#JB",
                    "lines": ["RAW_REPLACEMENT_SECRET"]
                }]
            }),
        )
        .await
        .unwrap_err();
    let message = error.to_string();

    // Then
    assert!(message.contains("[E_STALE_ANCHOR]"));
    assert!(message.contains("Re-read the file to get current anchors"));
    assert!(!message.contains("RAW_EXTERNAL_SECRET"));
    assert!(!message.contains("RAW_REPLACEMENT_SECRET"));
    assert!(!message.contains("Recovery attempted:"));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), external);
}

#[tokio::test]
async fn stale_recovery_conflict_preserves_original_error_and_guidance() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("conflict.txt");
    let original = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight";
    tokio::fs::write(&target, original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let read_result = read
        .execute(&ctx, json!({ "path": "conflict.txt" }))
        .await
        .unwrap();
    let stale_anchor = anchor_for_line(&read_result, 4);
    let external = "one\ntwo\nthree\nCONFLICT_EXTERNAL_SECRET\nfive\nsix\nseven\neight";
    tokio::fs::write(&target, external).await.unwrap();

    // When
    let error = edit
        .execute(
            &ctx,
            json!({
                "path": "conflict.txt",
                "edits": [{
                    "op": "replace",
                    "pos": stale_anchor,
                    "lines": ["CONFLICT_REPLACEMENT_SECRET"]
                }]
            }),
        )
        .await
        .unwrap_err();
    let message = error.to_string();

    // Then
    assert!(message.contains("[E_STALE_ANCHOR]"));
    assert!(message.contains("Re-read the file to get current anchors"));
    assert!(message.contains(
        "(Recovery attempted: your anchors match an older read of this file, but replaying that edit conflicts with changes made since. Re-read to get current anchors.)"
    ));
    assert!(!message.contains("CONFLICT_EXTERNAL_SECRET"));
    assert!(!message.contains("CONFLICT_REPLACEMENT_SECRET"));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), external);
}

#[tokio::test]
async fn stale_recovery_without_matching_history_preserves_guidance() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("target.txt");
    let other = workdir.join("other.txt");
    tokio::fs::write(&target, "target-1\ntarget-2\ntarget-3\ntarget-4")
        .await
        .unwrap();
    tokio::fs::write(&other, "other-1\nother-2\nother-3\nother-4")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let target_read = read
        .execute(&ctx, json!({ "path": "target.txt" }))
        .await
        .unwrap();
    let other_read = read
        .execute(&ctx, json!({ "path": "other.txt" }))
        .await
        .unwrap();
    let target_anchor = anchor_for_line(&target_read, 2);
    let other_anchor = anchor_for_line(&other_read, 2);
    assert_ne!(target_anchor, other_anchor);
    let target_before = tokio::fs::read_to_string(&target).await.unwrap();

    // When
    let error = edit
        .execute(
            &ctx,
            json!({
                "path": "target.txt",
                "edits": [{
                    "op": "replace",
                    "pos": other_anchor,
                    "lines": ["NO_HISTORY_REPLACEMENT_SECRET"]
                }]
            }),
        )
        .await
        .unwrap_err();
    let message = error.to_string();

    // Then
    assert!(message.contains("[E_STALE_ANCHOR]"));
    assert!(message.contains(
        "(Your anchors do not match any recent read of this file — they may be from a stale context or copied incorrectly. Re-read before editing.)"
    ));
    assert!(!message.contains("target-2"));
    assert!(!message.contains("other-2"));
    assert!(!message.contains("NO_HISTORY_REPLACEMENT_SECRET"));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        target_before
    );
    assert_eq!(
        tokio::fs::read_to_string(&other).await.unwrap(),
        "other-1\nother-2\nother-3\nother-4"
    );
}

#[tokio::test]
async fn exact_successful_payload_duplicate_is_rejected_without_mutation() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("duplicate.txt");
    tokio::fs::write(&target, "alpha\nbeta\ngamma")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let _ = read
        .execute(&ctx, json!({ "path": "duplicate.txt" }))
        .await
        .unwrap();
    let payload = json!({
        "path": "duplicate.txt",
        "edits": [{
            "op": "append",
            "lines": ["INSERTED_DUPLICATE_SECRET"]
        }]
    });

    // When
    let first = edit.execute(&ctx, payload.clone()).await.unwrap();
    let after_first = tokio::fs::read_to_string(&target).await.unwrap();
    let error = edit.execute(&ctx, payload).await.unwrap_err();
    let message = error.to_string();

    // Then
    assert_eq!(first["metadata"]["classification"], "applied");
    assert!(message.contains("[E_DUPLICATE_EDIT]"));
    assert!(!message.contains("INSERTED_DUPLICATE_SECRET"));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        after_first
    );
}

#[tokio::test]
async fn two_soft_noops_then_noop_loop_error_preserves_file() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("noop.txt");
    let original = "alpha\nbeta\ngamma";
    tokio::fs::write(&target, original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let read_result = read
        .execute(&ctx, json!({ "path": "noop.txt" }))
        .await
        .unwrap();
    let beta_anchor = anchor_for_line(&read_result, 2);
    let payload = json!({
        "path": "noop.txt",
        "edits": [{
            "op": "replace",
            "pos": beta_anchor,
            "lines": ["beta"]
        }]
    });

    // When
    let first = edit.execute(&ctx, payload.clone()).await.unwrap();
    let second = edit.execute(&ctx, payload.clone()).await.unwrap();
    let error = edit.execute(&ctx, payload).await.unwrap_err();
    let message = error.to_string();

    // Then
    assert_eq!(first["metadata"]["classification"], "noop");
    assert_eq!(second["metadata"]["classification"], "noop");
    assert!(message.contains("[E_NOOP_LOOP]"));
    assert!(!message.contains("beta"));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), original);
}

#[tokio::test]
async fn non_raw_read_resets_exact_duplicate_guard() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("duplicate-reset.txt");
    let marker = "INSERTED_AFTER_READ_SECRET";
    tokio::fs::write(&target, "alpha\nbeta").await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    read.execute(&ctx, json!({ "path": "duplicate-reset.txt" }))
        .await
        .unwrap();
    let payload = json!({
        "path": "duplicate-reset.txt",
        "edits": [{ "op": "append", "lines": [marker] }]
    });
    edit.execute(&ctx, payload.clone()).await.unwrap();
    let after_first = tokio::fs::read_to_string(&target).await.unwrap();

    // The exact replay is rejected before the second write.
    let duplicate_error = edit.execute(&ctx, payload.clone()).await.unwrap_err();
    let duplicate_message = duplicate_error.to_string();
    assert!(duplicate_message.contains("[E_DUPLICATE_EDIT]"));
    assert!(!duplicate_message.contains(marker));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        after_first
    );

    // When
    read.execute(&ctx, json!({ "path": "duplicate-reset.txt" }))
        .await
        .unwrap();
    let second = edit.execute(&ctx, payload).await.unwrap();

    // Then
    assert_eq!(second["metadata"]["classification"], "applied");
    let final_content = tokio::fs::read_to_string(&target).await.unwrap();
    assert_eq!(final_content.matches(marker).count(), 2);
}

#[tokio::test]
async fn non_raw_read_resets_noop_loop_guard() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("noop-reset.txt");
    let original = "alpha\nbeta\ngamma";
    tokio::fs::write(&target, original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let read_result = read
        .execute(&ctx, json!({ "path": "noop-reset.txt" }))
        .await
        .unwrap();
    let beta_anchor = anchor_for_line(&read_result, 2);
    let payload = json!({
        "path": "noop-reset.txt",
        "edits": [{ "op": "replace", "pos": beta_anchor, "lines": ["beta"] }]
    });
    edit.execute(&ctx, payload.clone()).await.unwrap();
    edit.execute(&ctx, payload.clone()).await.unwrap();

    // When
    read.execute(&ctx, json!({ "path": "noop-reset.txt" }))
        .await
        .unwrap();
    let third = edit.execute(&ctx, payload).await.unwrap();

    // Then
    assert_eq!(third["metadata"]["classification"], "noop");
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), original);
}

#[tokio::test]
async fn snapshots_are_isolated_by_session_workdir_and_target() {
    // Given
    let workdir = tempdir();
    let other_workdir = tempdir();
    let target = workdir.join("target.txt");
    let sibling = workdir.join("sibling.txt");
    let other_target = other_workdir.join("target.txt");
    let base = "one\ntwo\nthree\nfour\nfive";
    tokio::fs::write(&target, base).await.unwrap();
    tokio::fs::write(&sibling, base).await.unwrap();
    tokio::fs::write(
        &other_target,
        "one\nOTHER_WORKDIR_SECRET\nthree\nfour\nfive",
    )
    .await
    .unwrap();
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx_a = ctx_with_session(workdir.clone(), Some(session_a));
    let read_result = read
        .execute(&ctx_a, json!({ "path": "target.txt" }))
        .await
        .unwrap();
    let stale_line_three = anchor_for_line(&read_result, 3);

    // Session isolation: only session A has observed target.txt.
    let session_changed = "one\nSESSION_EXTERNAL_SECRET\nthree\nfour\nfive";
    tokio::fs::write(&target, session_changed).await.unwrap();
    let ctx_b = ctx_with_session(workdir.clone(), Some(session_b));
    let session_error = edit
        .execute(
            &ctx_b,
            json!({
                "path": "target.txt",
                "edits": [{ "op": "replace", "pos": stale_line_three, "lines": ["SESSION_REPLACEMENT_SECRET"] }]
            }),
        )
        .await
        .unwrap_err();
    let session_message = session_error.to_string();
    assert!(session_message.contains("[E_STALE_ANCHOR]"));
    assert!(session_message.contains("do not match any recent read"));
    assert!(!session_message.contains("SESSION_EXTERNAL_SECRET"));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        session_changed
    );

    // Target isolation: sibling.txt has no history from target.txt.
    let sibling_changed = "one\nTARGET_EXTERNAL_SECRET\nthree\nfour\nfive";
    tokio::fs::write(&sibling, sibling_changed).await.unwrap();
    let target_error = edit
        .execute(
            &ctx_a,
            json!({
                "path": "sibling.txt",
                "edits": [{ "op": "replace", "pos": stale_line_three, "lines": ["TARGET_REPLACEMENT_SECRET"] }]
            }),
        )
        .await
        .unwrap_err();
    let target_message = target_error.to_string();
    assert!(target_message.contains("[E_STALE_ANCHOR]"));
    assert!(target_message.contains("do not match any recent read"));
    assert!(!target_message.contains("TARGET_EXTERNAL_SECRET"));
    assert_eq!(
        tokio::fs::read_to_string(&sibling).await.unwrap(),
        sibling_changed
    );

    // Workdir isolation: the same session cannot recover across workdirs.
    let workdir_error = edit
        .execute(
            &ctx_with_session(other_workdir, Some(session_a)),
            json!({
                "path": "target.txt",
                "edits": [{ "op": "replace", "pos": stale_line_three, "lines": ["WORKDIR_REPLACEMENT_SECRET"] }]
            }),
        )
        .await
        .unwrap_err();
    let workdir_message = workdir_error.to_string();
    assert!(workdir_message.contains("[E_STALE_ANCHOR]"));
    assert!(workdir_message.contains("do not match any recent read"));
    assert!(!workdir_message.contains("OTHER_WORKDIR_SECRET"));
    assert!(!workdir_message.contains("WORKDIR_REPLACEMENT_SECRET"));
    assert_eq!(
        tokio::fs::read_to_string(&other_target).await.unwrap(),
        "one\nOTHER_WORKDIR_SECRET\nthree\nfour\nfive"
    );
}

#[tokio::test]
async fn edit_replaces_the_anchored_line_with_path_and_edits() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("notes.txt");
    tokio::fs::write(&target, "alpha\nbeta\ngamma\ndelta")
        .await
        .unwrap();
    let registry = ToolRegistry::builtins();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    // The literal anchor is copied from the pinned vector above. The test uses
    // only the public edit seam instead of deriving anchors through internals.

    // When
    edit.execute(
        &ctx,
        json!({
            "path": "notes.txt",
            "edits": [{
                "op": "replace",
                "pos": "2#JB",
                "lines": ["BETA"]
            }]
        }),
    )
    .await
    .unwrap();

    // Then
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        "alpha\nBETA\ngamma\ndelta"
    );
}
/// Prove that a failed non-raw Read does not clear a successful duplicate guard.
#[tokio::test]
async fn out_of_range_non_raw_read_preserves_duplicate_guard() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("duplicate-after-range.txt");
    tokio::fs::write(&target, "alpha\nbeta").await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let payload = json!({
        "path": "duplicate-after-range.txt",
        "edits": [{ "op": "append", "lines": ["marker"] }]
    });
    edit.execute(&ctx, payload.clone()).await.unwrap();
    let after_edit = tokio::fs::read_to_string(&target).await.unwrap();

    // When
    let range_error = read
        .execute(
            &ctx,
            json!({ "path": "duplicate-after-range.txt", "offset": 4 }),
        )
        .await
        .unwrap_err();
    let duplicate_error = edit.execute(&ctx, payload).await.unwrap_err();

    // Then
    assert_bad_read(range_error);
    assert!(matches!(
        duplicate_error,
        hya_tool::ToolError::Input(message)
            if message.starts_with("[E_DUPLICATE_EDIT]") && message.len() <= 1024
    ));
    assert_eq!(
        tokio::fs::read_to_string(&target).await.unwrap(),
        after_edit
    );
}

/// Prove that a failed non-raw Read does not clear the no-op loop guard.
#[tokio::test]
async fn out_of_range_non_raw_read_preserves_noop_guard() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("noop-after-range.txt");
    let original = "alpha\nbeta\ngamma";
    tokio::fs::write(&target, original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let read_result = read
        .execute(&ctx, json!({ "path": "noop-after-range.txt" }))
        .await
        .unwrap();
    let beta_anchor = anchor_for_line(&read_result, 2);
    let payload = json!({
        "path": "noop-after-range.txt",
        "edits": [{ "op": "replace", "pos": beta_anchor, "lines": ["beta"] }]
    });
    edit.execute(&ctx, payload.clone()).await.unwrap();
    edit.execute(&ctx, payload.clone()).await.unwrap();

    // When
    let range_error = read
        .execute(&ctx, json!({ "path": "noop-after-range.txt", "offset": 4 }))
        .await
        .unwrap_err();
    let loop_error = edit.execute(&ctx, payload).await.unwrap_err();

    // Then
    assert_bad_read(range_error);
    assert!(matches!(
        loop_error,
        hya_tool::ToolError::Input(message)
            if message.starts_with("[E_NOOP_LOOP]") && message.len() <= 1024
    ));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), original);
}

/// Prove that a failed non-raw Read does not create a recovery snapshot.
#[tokio::test]
async fn out_of_range_non_raw_read_does_not_add_recovery_history() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("history-after-range.txt");
    let anchor_source = workdir.join("history-anchor-source.txt");
    let original_lines = (1..=12)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>();
    let original = original_lines.join("\n");
    tokio::fs::write(&target, &original).await.unwrap();
    tokio::fs::write(&anchor_source, &original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let anchor_read = read
        .execute(&ctx, json!({ "path": "history-anchor-source.txt" }))
        .await
        .unwrap();
    let stale_line_two = anchor_for_line(&anchor_read, 2);
    let fresh_line_ten = anchor_for_line(&anchor_read, 10);

    // A range failure must not retain this target's bytes as a recovery base.
    let range_error = read
        .execute(
            &ctx,
            json!({ "path": "history-after-range.txt", "offset": 13 }),
        )
        .await
        .unwrap_err();
    let mut changed_lines = original_lines;
    changed_lines[0] = "changed-line-1".to_string();
    let external = changed_lines.join("\n");
    tokio::fs::write(&target, &external).await.unwrap();

    // When
    let recovery_error = edit
        .execute(
            &ctx,
            json!({
                "path": "history-after-range.txt",
                "edits": [
                    { "op": "replace", "pos": stale_line_two, "lines": ["line-2"] },
                    { "op": "replace", "pos": fresh_line_ten, "lines": ["line-10-edited"] }
                ]
            }),
        )
        .await
        .unwrap_err();

    // Then
    assert_bad_read(range_error);
    assert!(matches!(
        recovery_error,
        hya_tool::ToolError::Input(message)
            if message.starts_with("[E_STALE_ANCHOR]")
                && message.contains("do not match any recent read")
                && message.len() <= 1024
    ));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), external);
}

/// Prove that a pre-cancelled Read returns its typed error without state effects.
#[tokio::test]
async fn pre_cancelled_read_returns_cancelled_without_snapshot_effects() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("history-after-cancel.txt");
    let anchor_source = workdir.join("history-cancel-anchor-source.txt");
    let original_lines = (1..=12)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>();
    let original = original_lines.join("\n");
    tokio::fs::write(&target, &original).await.unwrap();
    tokio::fs::write(&anchor_source, &original).await.unwrap();
    let registry = ToolRegistry::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor_ctx = ctx_with(workdir.clone());
    let anchor_read = read
        .execute(
            &anchor_ctx,
            json!({ "path": "history-cancel-anchor-source.txt" }),
        )
        .await
        .unwrap();
    let stale_line_two = anchor_for_line(&anchor_read, 2);
    let fresh_line_ten = anchor_for_line(&anchor_read, 10);
    let cancelled_ctx = ctx_with(workdir);
    cancelled_ctx.cancel.cancel();

    // When
    let cancellation_error = read
        .execute(
            &cancelled_ctx,
            json!({ "path": "history-after-cancel.txt" }),
        )
        .await
        .unwrap_err();
    let mut changed_lines = original_lines;
    changed_lines[0] = "changed-line-1".to_string();
    let external = changed_lines.join("\n");
    tokio::fs::write(&target, &external).await.unwrap();
    let recovery_error = edit
        .execute(
            &anchor_ctx,
            json!({
                "path": "history-after-cancel.txt",
                "edits": [
                    { "op": "replace", "pos": stale_line_two, "lines": ["line-2"] },
                    { "op": "replace", "pos": fresh_line_ten, "lines": ["line-10-edited"] }
                ]
            }),
        )
        .await
        .unwrap_err();

    // Then
    assert!(matches!(cancellation_error, hya_tool::ToolError::Cancelled));
    assert!(matches!(
        recovery_error,
        hya_tool::ToolError::Input(message)
            if message.starts_with("[E_STALE_ANCHOR]")
                && message.contains("do not match any recent read")
                && message.len() <= 1024
    ));
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), external);
}

/// Prove that an oversized raw first line retains its continuation notice.
#[tokio::test]
async fn oversized_raw_first_line_retains_continuation_and_offsets() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("raw-oversize.txt");
    let first_line = "x".repeat(50 * 1024 + 1);
    tokio::fs::write(&target, format!("{first_line}\nsecond-line"))
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "raw-oversize.txt", "raw": true }))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();

    // Then
    assert!(output.len() <= 50 * 1024);
    assert!(output.contains("Showing lines 1-1 of 2"));
    assert!(output.contains("Use offset=2 to continue."));
    assert!(output.ends_with(']'));
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["nextOffset"], 2);
    assert_eq!(out["metadata"]["display"]["lineStart"], 1);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 1);
    assert_eq!(out["metadata"]["display"]["totalLines"], 2);
    assert_eq!(out["metadata"]["display"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["text"], out["content"]);
}

/// Prove that a near-cap hashline row is not cut by the result envelope.
#[tokio::test]
async fn near_cap_hashline_output_has_complete_metadata_and_notice() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("near-cap.txt");
    let line = "x".repeat(50 * 1024 - 10);
    tokio::fs::write(&target, line).await.unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "path": "near-cap.txt" }))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();

    // Then
    assert!(output.len() <= 50 * 1024);
    assert!(output.contains("Hashline output requires full lines"));
    assert!(output.ends_with("</content>"));
    assert_eq!(out["content"], "");
    assert_eq!(out["metadata"]["preview"], "");
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["text"], out["content"]);
    assert_eq!(out["metadata"]["display"]["lineStart"], 1);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 1);
    assert_eq!(out["metadata"]["display"]["totalLines"], 1);
    assert_eq!(out["metadata"]["display"]["truncated"], true);
    assert!(out["metadata"]["nextOffset"].is_null());
}
