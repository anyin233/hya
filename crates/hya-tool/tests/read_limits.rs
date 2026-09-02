//! `read` output caps and truncation behaviour.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, ToolResultPolicy, WebSearchPlane,
    cap_tool_output_with_policy,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-read-limits-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) =
        PermissionPlane::new(PermissionRules::new(vec![allow(Action::Read, "*")]));
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
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn read_truncates_individual_lines_to_open_code_limit() {
    // Given
    let workdir = tempdir();
    let long_line = "a".repeat(2001);
    tokio::fs::write(workdir.join("long.txt"), format!("{long_line}\n"))
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);
    let expected = format!("{}... (line truncated to 2000 chars)", "a".repeat(2000));

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "long.txt" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["content"], expected);
    assert_eq!(out["metadata"]["display"]["text"], expected);
    assert_eq!(out["metadata"]["truncated"], false);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains(&format!("1: {expected}"))
    );
}

#[tokio::test]
async fn read_caps_file_output_at_fifty_kilobytes() {
    // Given
    let workdir = tempdir();
    let line = "a".repeat(1000);
    let content = (0..60)
        .map(|_| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(workdir.join("large.txt"), content)
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "large.txt" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 50);
    assert_eq!(out["metadata"]["display"]["totalLines"], 60);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("(Output capped at 50 KB. Showing lines 1-50. Use offset=51 to continue.)")
    );
}

/// Preserve complete hashline rows and the Read wrapper when JSON escaping tightens the cap.
#[tokio::test]
async fn read_escape_heavy_rows_remain_complete_after_coding_cap() {
    const MAX_SERIALIZED_READ_BYTES: usize = 50 * 1024;

    // Given
    let workdir = tempdir();
    let payload = "\"\\\t\u{0001}".repeat(16);
    let source = (0..640)
        .map(|_| payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(source.len() > 40 * 1024);
    assert!(serde_json::to_vec(&source).unwrap().len() > MAX_SERIALIZED_READ_BYTES);
    tokio::fs::write(workdir.join("escaped.txt"), &source)
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let result = tool
        .execute(&ctx, json!({ "path": "escaped.txt", "limit": 640 }))
        .await
        .unwrap();
    let capped = cap_tool_output_with_policy(result, ToolResultPolicy::Coding);

    // Then
    let envelope = capped
        .as_object()
        .expect("Read result must remain an object");
    assert!(
        envelope
            .get("title")
            .is_some_and(serde_json::Value::is_string)
    );
    assert!(
        envelope
            .get("output")
            .is_some_and(serde_json::Value::is_string)
    );
    assert!(
        envelope
            .get("metadata")
            .is_some_and(serde_json::Value::is_object)
    );
    let metadata = capped["metadata"].as_object().expect("metadata object");
    assert_eq!(metadata["truncated"], true);
    assert!(metadata["nextOffset"].as_u64().is_some());
    assert_eq!(metadata["display"]["truncated"], true);

    let output = capped["output"].as_str().expect("Read output must be text");
    assert!(output.ends_with("</content>"));
    let body = output
        .split_once("<content>\n")
        .map(|(_, body)| body)
        .expect("Read output must include its content wrapper")
        .strip_suffix("</content>")
        .expect("Read output must close its content wrapper")
        .strip_suffix('\n')
        .expect("Read output must separate the wrapper footer");
    let (rows_text, continuation) = body
        .rsplit_once("\n\n[Showing lines ")
        .expect("capped Read output must retain a continuation notice");
    assert!(continuation.contains("Use offset="));
    assert!(continuation.ends_with(']'));
    let rows = rows_text.split('\n').collect::<Vec<_>>();
    assert!(!rows.is_empty());
    assert!(rows.len() < 640);
    for (index, row) in rows.iter().enumerate() {
        let (anchor, text) = row.split_once(':').expect("hashline row must be complete");
        let (line, hash) = anchor
            .trim()
            .split_once('#')
            .expect("hashline row must contain its anchor");
        assert_eq!(line.parse::<usize>().unwrap(), index + 1);
        assert_eq!(hash.len(), 2);
        assert_eq!(text, payload);
    }

    let content = capped["content"].as_str().expect("content text");
    let display_text = capped["metadata"]["display"]["text"]
        .as_str()
        .expect("display text");
    assert_eq!(display_text, content);
    assert!(!content.is_empty());
    assert!(content.len() < source.len());
    assert!(content.split('\n').all(|line| line == payload));
    for value in [
        &capped["output"],
        &capped["content"],
        &capped["metadata"]["display"]["text"],
    ] {
        assert!(serde_json::to_vec(value).unwrap().len() <= MAX_SERIALIZED_READ_BYTES);
    }
    assert!(serde_json::to_vec(&capped).unwrap().len() <= 256 * 1024);
}
