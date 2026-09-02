//! Focused native Grep contracts for hashline output and recovery.
//!
//! These tests use only the public `ToolRegistry` and `ToolCtx` seams. They
//! exercise the pinned `pattern`/`path`/`glob` search surface, bounded result
//! metadata, and the shared Grep-to-Edit snapshot workflow.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, Decision, InteractionPlane, LspPlane, Mode, PermissionInterceptor, PermissionPlane,
    PermissionRules, Resource, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError,
    ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const MAX_GREP_LOGICAL_LINE_BYTES: usize = 1024 * 1024;
const MAX_GLOB_BYTES: usize = 4096;
const MAX_IGNORE_RULES: usize = 10_000;
const MAX_WARNING_BYTES: usize = 4096;

/// Record the first resource requested by a denied permission check.
struct RecordingPermissionInterceptor {
    requests: Arc<Mutex<Vec<(Action, String)>>>,
}

#[async_trait::async_trait]
impl PermissionInterceptor for RecordingPermissionInterceptor {
    async fn intercept(
        &self,
        _session: Option<hya_proto::SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        let request = (action, resource.pattern());
        match self.requests.lock() {
            Ok(mut requests) => requests.push(request),
            Err(poisoned) => poisoned.into_inner().push(request),
        }
        Some(Decision::Reject { feedback: None })
    }
}

/// Build an allow rule for one tool or resource action.
fn allow(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Allow)
}

/// Build a deny rule for one tool or resource action.
fn deny(action: Action, pattern: &str) -> Rule {
    Rule::new(action, pattern, Mode::Deny)
}

/// Create an isolated temporary workdir for one public tool scenario.
fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-grep-hashline-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Construct a permissive Grep/Read/Edit context with a fresh cancellation token.
fn ctx_with(workdir: PathBuf) -> ToolCtx {
    ctx_with_components(
        workdir,
        CancellationToken::new(),
        vec![
            allow(Action::Read, "*"),
            allow(Action::Edit, "*"),
            allow(Action::Grep, "*"),
            allow(Action::ExternalDirectory, "*"),
        ],
    )
}

/// Construct a context with custom permission rules and cancellation.
fn ctx_with_components(workdir: PathBuf, cancel: CancellationToken, rules: Vec<Rule>) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir,
        cancel,
    }
}

/// Return the rendered display rows for the group whose path has `suffix`.
fn display_rows_for<'a>(result: &'a Value, suffix: &str) -> &'a [Value] {
    let groups = result["metadata"]["display"]["groups"]
        .as_array()
        .expect("Grep display groups must be an array");
    let group = groups
        .iter()
        .find(|group| {
            group["path"]
                .as_str()
                .is_some_and(|path| path.ends_with(suffix))
        })
        .unwrap_or_else(|| panic!("Grep display has no group ending in {suffix:?}: {groups:?}"));
    group["rows"]
        .as_array()
        .map(Vec::as_slice)
        .expect("Grep display group rows must be an array")
}

/// Return all display group paths in their emitted order.
fn display_paths(result: &Value) -> Vec<&str> {
    result["metadata"]["display"]["groups"]
        .as_array()
        .expect("Grep display groups must be an array")
        .iter()
        .map(|group| {
            group["path"]
                .as_str()
                .expect("Grep display group path must be a string")
        })
        .collect()
}

/// Extract an emitted `LINE#HASH` anchor for one displayed source line.
fn grep_anchor(result: &Value, line: usize, text: &str) -> String {
    let output = result["output"]
        .as_str()
        .expect("Grep result output must be a string");
    output
        .lines()
        .find_map(|row| {
            let row = row.trim_start();
            let (prefix, content) = row.split_once(':')?;
            let (line_number, hash) = prefix.split_once('#')?;
            let parsed_line = line_number.parse::<usize>().ok()?;
            if parsed_line == line && !hash.is_empty() && content == text {
                Some(prefix.to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("Grep output has no anchor for line {line}: {output:?}"))
}

/// Assert one semantic display row without depending on hashline rendering.
fn assert_display_row(row: &Value, line: u64, text: &str, is_match: bool) {
    assert_eq!(row["line"], line);
    assert_eq!(row["text"], text);
    assert_eq!(row["isMatch"], is_match);
}

/// Verify native regex matching and the independent case-sensitivity switch.
#[tokio::test]
async fn grep_regex_respects_case_mode() {
    let workdir = tempdir();
    tokio::fs::write(
        workdir.join("cases.txt"),
        "needle lower\nNEEDLE upper\nother\n",
    )
    .await
    .unwrap();
    let registry = ToolRegistry::builtins();
    let tool = registry.get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let sensitive = tool
        .execute(&ctx, json!({ "pattern": "^needle", "path": "cases.txt" }))
        .await
        .unwrap();
    let insensitive = tool
        .execute(
            &ctx,
            json!({
                "pattern": "^needle",
                "path": "cases.txt",
                "ignoreCase": true
            }),
        )
        .await
        .unwrap();

    assert_eq!(sensitive["metadata"]["matches"], 1);
    assert_eq!(insensitive["metadata"]["matches"], 2);
    assert_eq!(display_rows_for(&sensitive, "cases.txt").len(), 1);
    assert_eq!(display_rows_for(&insensitive, "cases.txt").len(), 2);
}

/// Verify glob filtering for nested files under a directory target.
#[tokio::test]
async fn grep_glob_filters_nested_files_for_directory_targets() {
    let workdir = tempdir();
    let src = workdir.join("src");
    let nested = src.join("nested");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(src.join("top.rs"), "needle top\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("readme.md"), "needle docs\n")
        .await
        .unwrap();
    tokio::fs::write(nested.join("deep.rs"), "needle deep\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(
            &ctx,
            json!({
                "pattern": "needle",
                "path": "src",
                "glob": "**/*.rs"
            }),
        )
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 2);
    assert_eq!(result["metadata"]["files"], 2);
    let paths = display_paths(&result);
    assert_eq!(paths.len(), 2);
    assert!(paths[0].ends_with("nested/deep.rs"));
    assert!(paths[1].ends_with("top.rs"));
    let output = result["output"].as_str().unwrap();
    assert!(!output.contains("readme.md"));
}

/// Verify gitignore traversal and skipping of binary or unloadable files.
#[tokio::test]
async fn grep_honors_gitignore_and_skips_binary_and_unloadable_files() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join(".gitignore"), "ignored.txt\nignored-dir/\n")
        .await
        .unwrap();
    tokio::fs::create_dir_all(workdir.join("ignored-dir"))
        .await
        .unwrap();
    tokio::fs::write(workdir.join("visible.txt"), "needle visible\n")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("ignored.txt"), "needle ignored\n")
        .await
        .unwrap();
    tokio::fs::write(
        workdir.join("ignored-dir").join("nested.txt"),
        "needle ignored directory\n",
    )
    .await
    .unwrap();
    tokio::fs::write(workdir.join("binary.dat"), b"needle\0binary\n")
        .await
        .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("missing-target", workdir.join("unloadable.txt")).unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(&ctx, json!({ "pattern": "needle" }))
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 1);
    assert_eq!(result["metadata"]["files"], 1);
    let paths = display_paths(&result);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("visible.txt"));
    let output = result["output"].as_str().unwrap();
    for omitted in ["ignored.txt", "ignored-dir", "binary.dat"] {
        assert!(
            !output.contains(omitted),
            "skipped path leaked into output: {omitted}"
        );
    }
    #[cfg(unix)]
    assert!(!output.contains("unloadable.txt"));
}

/// Verify the exact no-match result message and empty structured display.
#[tokio::test]
async fn grep_returns_no_matches_without_display_groups() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("empty.txt"), "nothing relevant\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(&ctx, json!({ "pattern": "absent", "path": "empty.txt" }))
        .await
        .unwrap();

    assert_eq!(result["title"], "absent");
    assert_eq!(result["output"], "No matches found for absent.");
    assert_eq!(result["metadata"]["matches"], 0);
    assert_eq!(result["metadata"]["files"], 0);
    assert_eq!(result["metadata"]["truncated"], false);
    assert_eq!(result["metadata"]["display"]["groups"], json!([]));
}

/// Verify that overlapping or adjacent context ranges render as one region.
#[tokio::test]
async fn grep_merges_overlapping_context_ranges_without_separator() {
    let workdir = tempdir();
    tokio::fs::write(
        workdir.join("merge.txt"),
        "one\nneedle-two\nthree\nneedle-four\nfive\nsix\nseven\n",
    )
    .await
    .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "merge.txt", "context": 1 }),
        )
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 2);
    assert_eq!(result["metadata"]["files"], 1);
    let rows = display_rows_for(&result, "merge.txt");
    assert_eq!(rows.len(), 5);
    for (row, (line, text, is_match)) in rows.iter().zip([
        (1, "one", false),
        (2, "needle-two", true),
        (3, "three", false),
        (4, "needle-four", true),
        (5, "five", false),
    ]) {
        assert_display_row(row, line, text, is_match);
    }
    let output = result["output"].as_str().unwrap();
    assert_eq!(
        output.lines().filter(|line| line.trim() == "...").count(),
        0,
        "merged context ranges must not contain a separator"
    );
}

/// Verify that disjoint context ranges have exactly one visible separator.
#[tokio::test]
async fn grep_separates_disjoint_context_ranges() {
    let workdir = tempdir();
    let source = (1..=10)
        .map(|line| match line {
            2 => "needle-two".to_string(),
            8 => "needle-eight".to_string(),
            _ => format!("line-{line}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(workdir.join("separate.txt"), source)
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(
            &ctx,
            json!({
                "pattern": "needle",
                "path": "separate.txt",
                "context": 1
            }),
        )
        .await
        .unwrap();

    let rows = display_rows_for(&result, "separate.txt");
    assert_eq!(
        rows.iter()
            .map(|row| row["line"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 7, 8, 9]
    );
    let output = result["output"].as_str().unwrap();
    assert_eq!(
        output.lines().filter(|line| line.trim() == "...").count(),
        1,
        "one separator is required between the two disjoint ranges"
    );
}

/// Verify lexical file and row order independent of creation order.
#[tokio::test]
async fn grep_emits_deterministic_file_and_row_order() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("z.txt"), "needle-z-first\nneedle-z-second\n")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("a.txt"), "needle-a-first\nneedle-a-second\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(&ctx, json!({ "pattern": "needle" }))
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 4);
    assert_eq!(result["metadata"]["files"], 2);
    let paths = display_paths(&result);
    assert_eq!(paths.len(), 2);
    assert!(paths[0].ends_with("a.txt"));
    assert!(paths[1].ends_with("z.txt"));
    for path in ["a.txt", "z.txt"] {
        let rows = display_rows_for(&result, path);
        assert_eq!(
            rows.iter()
                .map(|row| row["line"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}

/// Verify cwd defaults, zero context, and the default fifty-match limit.
#[tokio::test]
async fn grep_defaults_to_workdir_zero_context_and_fifty_matches() {
    let workdir = tempdir();
    let source = (1..=51)
        .map(|line| format!("needle-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(workdir.join("default.txt"), source)
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(&ctx, json!({ "pattern": "needle" }))
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 50);
    assert_eq!(result["metadata"]["files"], 1);
    assert_eq!(result["metadata"]["truncated"], true);
    assert_eq!(display_rows_for(&result, "default.txt").len(), 50);
    assert_eq!(
        result["output"]
            .as_str()
            .unwrap()
            .lines()
            .filter(|line| line.trim() == "...")
            .count(),
        0,
        "default context is zero"
    );
}

/// Verify that an exact limit is complete while one fewer observes truncation.
#[tokio::test]
async fn grep_limit_boundary_distinguishes_exact_limit_from_limit_plus_one() {
    let workdir = tempdir();
    tokio::fs::write(
        workdir.join("limit.txt"),
        "needle-one\nneedle-two\nneedle-three\n",
    )
    .await
    .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let exact = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "limit.txt", "limit": 3 }),
        )
        .await
        .unwrap();
    let truncated = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "limit.txt", "limit": 2 }),
        )
        .await
        .unwrap();

    assert_eq!(exact["metadata"]["matches"], 3);
    assert_eq!(exact["metadata"]["files"], 1);
    assert_eq!(exact["metadata"]["truncated"], false);
    assert_eq!(display_rows_for(&exact, "limit.txt").len(), 3);
    assert_eq!(truncated["metadata"]["matches"], 2);
    assert_eq!(truncated["metadata"]["files"], 1);
    assert_eq!(truncated["metadata"]["truncated"], true);
    assert_eq!(display_rows_for(&truncated, "limit.txt").len(), 2);
    assert!(
        !truncated["output"]
            .as_str()
            .unwrap()
            .contains("needle-three")
    );
}

/// Verify the closed runtime input rejects unknown fields.
#[tokio::test]
async fn grep_rejects_unknown_fields() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let error = tool
        .execute(&ctx, json!({ "pattern": "needle", "unexpected": true }))
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Input(_)));
}

/// Verify context values above the published maximum are rejected.
#[tokio::test]
async fn grep_rejects_context_above_bound() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let error = tool
        .execute(&ctx, json!({ "pattern": "needle", "context": 6 }))
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Input(_)));
}

/// Verify a zero match limit is rejected by the positive lower bound.
#[tokio::test]
async fn grep_rejects_zero_limit() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let error = tool
        .execute(&ctx, json!({ "pattern": "needle", "limit": 0 }))
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Input(_)));
}

/// Verify a match limit above the published maximum is rejected.
#[tokio::test]
async fn grep_rejects_limit_above_bound() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let error = tool
        .execute(&ctx, json!({ "pattern": "needle", "limit": 201 }))
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Input(_)));
}

/// Verify an invalid native regex is reported as typed input failure.
#[tokio::test]
async fn grep_rejects_invalid_regex() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let error = tool
        .execute(&ctx, json!({ "pattern": "[unterminated" }))
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Input(_)));
}

/// Verify the external-directory permission gate remains before traversal.
#[tokio::test]
async fn grep_requires_external_directory_permission() {
    let workdir = tempdir();
    let outside = tempdir();
    tokio::fs::write(outside.join("outside.txt"), "needle\n")
        .await
        .unwrap();
    let ctx = ctx_with_components(
        workdir,
        CancellationToken::new(),
        vec![
            allow(Action::Grep, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
    );
    let tool = ToolRegistry::builtins().get("grep").unwrap();

    let error = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": outside.to_string_lossy() }),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Permission(_)));
}

/// Verify hashline anchors and typed display rows come from the same result.
#[tokio::test]
async fn grep_emits_hashline_rows_and_display_match_flags() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("display.rs"), "before\nneedle here\nafter\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let result = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "display.rs", "context": 1 }),
        )
        .await
        .unwrap();

    let anchor = grep_anchor(&result, 2, "needle here");
    let (line, hash) = anchor.split_once('#').unwrap();
    assert_eq!(line, "2");
    assert_eq!(hash.len(), 2);
    assert!(
        hash.chars()
            .all(|character| "ZPMQVRWSNKTXJBYH".contains(character))
    );
    assert!(
        result["output"]
            .as_str()
            .unwrap()
            .lines()
            .any(|row| row.trim_start().starts_with("2#") && row.ends_with(":needle here"))
    );
    let summary = result["output"].as_str().unwrap().lines().last().unwrap();
    assert!(summary.contains("match") && summary.contains("file"));

    let rows = display_rows_for(&result, "display.rs");
    assert_eq!(rows.len(), 3);
    assert_display_row(&rows[0], 1, "before", false);
    assert_display_row(&rows[1], 2, "needle here", true);
    assert_display_row(&rows[2], 3, "after", false);
}

/// Verify a pre-cancelled Grep cannot seed recovery state for a later Edit.
#[tokio::test]
async fn grep_cancellation_has_no_snapshot_side_effects() {
    let workdir = tempdir();
    let target = workdir.join("cancelled.txt");
    tokio::fs::write(&target, "before\nneedle\nafter\n")
        .await
        .unwrap();

    let anchor_registry = ToolRegistry::builtins();
    let anchor_read = anchor_registry.get("read").unwrap();
    let anchor_ctx = ctx_with(workdir.clone());
    let read_result = anchor_read
        .execute(&anchor_ctx, json!({ "path": "cancelled.txt" }))
        .await
        .unwrap();
    let anchor = grep_anchor(
        &json!({ "output": read_result["output"].clone() }),
        2,
        "needle",
    );

    let registry = ToolRegistry::builtins();
    let grep = registry.get("grep").unwrap();
    let edit = registry.get("edit").unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled_ctx = ctx_with_components(
        workdir.clone(),
        cancel,
        vec![
            allow(Action::Read, "*"),
            allow(Action::Edit, "*"),
            allow(Action::Grep, "*"),
            allow(Action::ExternalDirectory, "*"),
        ],
    );

    let cancellation = grep
        .execute(
            &cancelled_ctx,
            json!({ "pattern": "needle", "path": "cancelled.txt" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(cancellation, ToolError::Cancelled));

    tokio::fs::write(&target, "external\nneedle\nafter\n")
        .await
        .unwrap();
    let active_ctx = ctx_with(workdir);
    let error = edit
        .execute(
            &active_ctx,
            json!({
                "path": "cancelled.txt",
                "edits": [{ "op": "replace", "pos": anchor, "lines": ["replaced"] }]
            }),
        )
        .await
        .unwrap_err();
    let message = match error {
        ToolError::Input(message) => message,
        other => panic!("stale anchor must remain a typed input error: {other:?}"),
    };
    assert!(message.contains("[E_STALE_ANCHOR]"));
    assert!(!message.contains("Recovery attempted"));
    assert_eq!(
        tokio::fs::read_to_string(target).await.unwrap(),
        "external\nneedle\nafter\n"
    );
}

/// Verify Edit recovers a stale anchor emitted by Grep after an external change
/// outside the exact context-three merge window.
#[tokio::test]
async fn grep_to_edit_recovers_from_actual_emitted_anchor() {
    let workdir = tempdir();
    let target = workdir.join("recovery.txt");
    let source = (1..=12)
        .map(|line| match line {
            3 => "needle-noop".to_string(),
            10 => "needle-target".to_string(),
            _ => format!("line-{line}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(&target, &source).await.unwrap();

    let registry = ToolRegistry::builtins();
    let grep = registry.get("grep").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(workdir);
    let grep_result = grep
        .execute(&ctx, json!({ "pattern": "needle", "path": "recovery.txt" }))
        .await
        .unwrap();
    let stale_noop_anchor = grep_anchor(&grep_result, 3, "needle-noop");
    let target_anchor = grep_anchor(&grep_result, 10, "needle-target");

    let mut changed = (1..=12)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>();
    changed[1] = "external-far-change".to_string();
    changed[2] = "needle-noop".to_string();
    changed[9] = "needle-target".to_string();
    tokio::fs::write(&target, changed.join("\n")).await.unwrap();

    let edit_result = edit
        .execute(
            &ctx,
            json!({
                "path": "recovery.txt",
                "edits": [
                    { "op": "replace", "pos": stale_noop_anchor, "lines": ["needle-noop"] },
                    { "op": "replace", "pos": target_anchor, "lines": ["needle-replaced"] }
                ]
            }),
        )
        .await
        .unwrap();

    let final_text = tokio::fs::read_to_string(&target).await.unwrap();
    let final_lines = final_text.lines().collect::<Vec<_>>();
    assert_eq!(final_lines[1], "external-far-change");
    assert_eq!(final_lines[2], "needle-noop");
    assert_eq!(final_lines[9], "needle-replaced");
    assert!(
        edit_result["output"]
            .as_str()
            .unwrap()
            .contains("Recovered stale anchors")
    );
}

/// Verify an over-budget newline-free line is skipped with one bounded warning
/// and that a pre-cancelled scan completes without doing filesystem work.
#[tokio::test]
async fn grep_skips_over_budget_line_with_warning_and_observes_cancellation() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let workdir = tempdir();
        let mut contents = vec![b'x'; MAX_GREP_LOGICAL_LINE_BYTES + 1];
        contents.extend_from_slice(b"\nnormal visible NEEDLE line\n");
        tokio::fs::write(workdir.join("oversized.txt"), contents)
            .await
            .unwrap();

        let registry = ToolRegistry::builtins();
        let grep = registry.get("grep").unwrap();
        let result = grep
            .execute(&ctx_with(workdir.clone()), json!({ "pattern": "NEEDLE" }))
            .await
            .expect("an over-budget line must not fail the whole search");

        assert_eq!(result["metadata"]["matches"], 1);
        assert_eq!(result["metadata"]["files"], 1);
        assert!(
            result["output"]
                .as_str()
                .is_some_and(|output| output.contains("normal visible NEEDLE line"))
        );
        let warnings = result["metadata"]["warnings"]
            .as_array()
            .expect("skipping an over-budget line must produce warnings");
        assert_eq!(warnings.len(), 1);
        let warning = warnings[0].as_str().expect("Grep warnings must be strings");
        assert!(warning.len() <= MAX_WARNING_BYTES);
        assert!(warning.contains("oversized.txt"));

        let cancel = CancellationToken::new();
        cancel.cancel();
        let cancelled = grep
            .execute(
                &ctx_with_components(
                    workdir,
                    cancel,
                    vec![
                        allow(Action::Read, "*"),
                        allow(Action::Edit, "*"),
                        allow(Action::Grep, "*"),
                        allow(Action::ExternalDirectory, "*"),
                    ],
                ),
                json!({ "pattern": "NEEDLE", "path": "oversized.txt" }),
            )
            .await;
        assert!(matches!(cancelled, Err(ToolError::Cancelled)));
    })
    .await
    .expect("bounded Grep and cancellation must finish within the outer timeout");
}

/// A matched line that cannot fit as one hashline row reports explicit display truncation.
#[tokio::test]
async fn grep_marks_over_budget_rendered_context_instead_of_silently_omitting_it() {
    let workdir = tempdir();
    let mut line = String::from("needle-");
    line.push_str(&"x".repeat(60 * 1024));
    tokio::fs::write(workdir.join("large-match.txt"), line)
        .await
        .unwrap();
    let grep = ToolRegistry::builtins().get("grep").unwrap();

    let result = grep
        .execute(&ctx_with(workdir), json!({ "pattern": "needle" }))
        .await
        .unwrap();

    assert_eq!(result["metadata"]["matches"], 1);
    assert_eq!(result["metadata"]["files"], 0);
    assert_eq!(result["metadata"]["displayTruncated"], true);
    assert_eq!(
        result["metadata"]["display"]["groups"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let output = result["output"].as_str().unwrap();
    assert!(output.contains("omitted"));
    assert!(output.len() <= 50 * 1024);
}

/// Verify Grep's caller-provided glob accepts both standard negated classes.
#[tokio::test]
async fn grep_caller_glob_supports_negated_character_classes() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        for name in ["a.txt", "b.txt", "c.txt"] {
            tokio::fs::write(workdir.join(name), "needle\n")
                .await
                .unwrap();
        }
        let registry = ToolRegistry::builtins();
        let grep = registry.get("grep").unwrap();
        let ctx = ctx_with(workdir);

        for glob in ["[!a].txt", "[^a].txt"] {
            let result = grep
                .execute(&ctx, json!({ "pattern": "needle", "glob": glob }))
                .await
                .unwrap();
            assert_eq!(result["metadata"]["matches"], 2, "glob {glob:?}");
            assert_eq!(result["metadata"]["files"], 2, "glob {glob:?}");
            let paths = display_paths(&result);
            assert_eq!(paths.len(), 2, "glob {glob:?}");
            assert!(paths.iter().all(|path| !path.ends_with("a.txt")));
            assert!(paths.iter().any(|path| path.ends_with("b.txt")));
            assert!(paths.iter().any(|path| path.ends_with("c.txt")));
        }
    })
    .await
    .expect("negated-class Grep cases must finish within the outer timeout");
}

/// Verify ignore files use the same negated character-class semantics as globs.
#[tokio::test]
async fn grep_ignore_rules_support_negated_character_classes() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        tokio::fs::write(workdir.join(".gitignore"), "[!a].txt\n")
            .await
            .unwrap();
        tokio::fs::write(workdir.join(".ignore"), "[^a].log\n")
            .await
            .unwrap();
        for name in ["a.txt", "b.txt", "a.log", "b.log"] {
            tokio::fs::write(workdir.join(name), "needle\n")
                .await
                .unwrap();
        }

        let grep = ToolRegistry::builtins().get("grep").unwrap();
        let result = grep
            .execute(&ctx_with(workdir), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        assert_eq!(result["metadata"]["matches"], 2);
        assert_eq!(result["metadata"]["files"], 2);
        let paths = display_paths(&result);
        assert!(paths.iter().any(|path| path.ends_with("a.txt")));
        assert!(paths.iter().any(|path| path.ends_with("a.log")));
        assert!(
            paths
                .iter()
                .all(|path| { !path.ends_with("b.txt") && !path.ends_with("b.log") })
        );
    })
    .await
    .expect("negated-class ignore cases must finish within the outer timeout");
}

/// Verify a caller-provided Grep glob cannot dimension unbounded matching work.
#[tokio::test]
async fn grep_rejects_over_budget_glob_input() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("target.txt"), "needle\n")
            .await
            .unwrap();
        let glob = "x".repeat(MAX_GLOB_BYTES + 1);
        let grep = ToolRegistry::builtins().get("grep").unwrap();
        let error = grep
            .execute(
                &ctx_with(workdir),
                json!({ "pattern": "needle", "path": "target.txt", "glob": glob }),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Input(message)
                if message.contains("glob") && message.contains("4096")),
            "oversized Grep glob must be a bounded input error: {error:?}"
        );
    })
    .await
    .expect("oversized Grep glob validation must finish within the outer timeout");
}

/// Verify an ignore file over the rule budget is skipped with one bounded
/// warning while later files remain searchable.
#[tokio::test]
async fn grep_bounds_oversized_ignore_rules_with_one_warning() {
    tokio::time::timeout(Duration::from_secs(3), async {
        let workdir = tempdir();
        let rules = (0..=MAX_IGNORE_RULES)
            .map(|index| format!("not-present-{index}.txt"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(workdir.join(".gitignore"), rules)
            .await
            .unwrap();
        tokio::fs::write(workdir.join("visible.txt"), "needle\n")
            .await
            .unwrap();

        let grep = ToolRegistry::builtins().get("grep").unwrap();
        let result = grep
            .execute(&ctx_with(workdir), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        assert_eq!(result["metadata"]["matches"], 1);
        assert_eq!(result["metadata"]["files"], 1);
        let paths = display_paths(&result);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("visible.txt"));
        let warnings = result["metadata"]["warnings"]
            .as_array()
            .expect("over-budget ignore input must produce a warning");
        assert_eq!(warnings.len(), 1);
        let warning = warnings[0].as_str().expect("Grep warnings must be strings");
        assert!(warning.len() <= MAX_WARNING_BYTES);
        assert!(warning.contains(".gitignore"));
    })
    .await
    .expect("bounded ignore parsing must finish within the outer timeout");
}

/// Verify external Grep permission asks use one lexical resource before file
/// versus directory metadata can change the requested resource.
#[tokio::test]
async fn grep_external_permission_resource_is_file_directory_blind() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        let outside = tempdir();
        let outside_file = outside.join("outside.txt");
        let outside_directory = outside.join("outside-dir");
        tokio::fs::write(&outside_file, "needle\n").await.unwrap();
        tokio::fs::create_dir(&outside_directory).await.unwrap();
        let grep = ToolRegistry::builtins().get("grep").unwrap();

        let file_requests = Arc::new(Mutex::new(Vec::new()));
        let mut file_ctx = ctx_with_components(
            workdir.clone(),
            CancellationToken::new(),
            vec![allow(Action::Grep, "*")],
        );
        file_ctx.permission = file_ctx.permission.clone().with_interceptor(Arc::new(
            RecordingPermissionInterceptor {
                requests: Arc::clone(&file_requests),
            },
        ));
        let file_error = grep
            .execute(
                &file_ctx,
                json!({ "pattern": "needle", "path": outside_file }),
            )
            .await
            .unwrap_err();
        assert!(matches!(file_error, ToolError::Permission(_)));

        let directory_requests = Arc::new(Mutex::new(Vec::new()));
        let mut directory_ctx = ctx_with_components(
            workdir,
            CancellationToken::new(),
            vec![allow(Action::Grep, "*")],
        );
        directory_ctx.permission = directory_ctx.permission.clone().with_interceptor(Arc::new(
            RecordingPermissionInterceptor {
                requests: Arc::clone(&directory_requests),
            },
        ));
        let directory_error = grep
            .execute(
                &directory_ctx,
                json!({ "pattern": "needle", "path": outside_directory }),
            )
            .await
            .unwrap_err();
        assert!(matches!(directory_error, ToolError::Permission(_)));

        let file_requests = match file_requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let directory_requests = match directory_requests.lock() {
            Ok(requests) => requests.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        assert_eq!(file_requests.len(), 1);
        assert_eq!(directory_requests.len(), 1);
        assert_eq!(file_requests[0].0, Action::ExternalDirectory);
        assert_eq!(directory_requests[0].0, Action::ExternalDirectory);
        assert_eq!(
            file_requests[0].1, directory_requests[0].1,
            "the first external permission resource must not reveal target kind"
        );
    })
    .await
    .expect("external permission checks must finish within the outer timeout");
}
