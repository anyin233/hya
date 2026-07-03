#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-read-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    ctx_with_rules(vec![allow(Action::Read, "*")], workdir)
}

fn ctx_with_rules(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: hya_tool::AgentCatalogPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn read_supports_file_path_offset_limit_and_open_code_display_metadata() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir.clone());

    // When
    let out = tool
        .execute(
            &ctx,
            json!({ "filePath": "notes.txt", "offset": 2, "limit": 2 }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "notes.txt");
    assert_eq!(out["content"], "beta\ngamma");
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("<type>file</type>")
    );
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("2: beta\n3: gamma")
    );
    assert_eq!(out["metadata"]["display"]["type"], "file");
    assert_eq!(out["metadata"]["display"]["lineStart"], 2);
    assert_eq!(out["metadata"]["display"]["lineEnd"], 3);
    assert_eq!(out["metadata"]["display"]["totalLines"], 4);
    assert_eq!(out["metadata"]["truncated"], true);
    assert_eq!(out["metadata"]["preview"], "beta\ngamma");
    assert_eq!(out["metadata"]["loaded"], json!([]));
}

#[tokio::test]
async fn read_lists_directories_with_sorted_entries_and_open_code_metadata() {
    // Given
    let workdir = tempdir();
    tokio::fs::create_dir_all(workdir.join("dir/sub"))
        .await
        .unwrap();
    tokio::fs::write(workdir.join("dir/b.txt"), "b")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("dir/a.txt"), "a")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "dir", "offset": 1, "limit": 2 }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "dir");
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("<type>directory</type>")
    );
    assert_eq!(out["metadata"]["display"]["type"], "directory");
    assert_eq!(
        out["metadata"]["display"]["entries"],
        json!(["sub/", "a.txt"])
    );
    assert_eq!(out["metadata"]["display"]["totalEntries"], 3);
    assert_eq!(out["metadata"]["display"]["truncated"], true);
}

#[tokio::test]
async fn read_rejects_offset_beyond_file_line_count() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "one\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let result = tool
        .execute(&ctx, json!({ "filePath": "notes.txt", "offset": 3 }))
        .await;

    // Then
    assert!(
        matches!(result, Err(ToolError::Other(message)) if message == "Offset 3 is out of range for this file (1 lines)")
    );
}

#[tokio::test]
async fn read_requires_external_directory_permission_for_outside_file_path() {
    // Given
    let workdir = tempdir();
    let outside_dir = tempdir();
    let outside = outside_dir.join("outside.txt");
    tokio::fs::write(&outside, "secret\n").await.unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with_rules(
        vec![
            allow(Action::Read, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        workdir,
    );

    // When
    let result = tool
        .execute(&ctx, json!({ "filePath": outside.to_string_lossy() }))
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
}

#[tokio::test]
async fn read_strips_utf8_bom_from_file_output() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("bom.txt"), b"\xEF\xBB\xBFhello\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "bom.txt" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["content"], "hello");
    assert_eq!(out["metadata"]["display"]["text"], "hello");
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("1: hello\n\n(End of file - total 1 lines)")
    );
}

#[tokio::test]
async fn read_returns_open_code_attachment_for_png_files() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(
        workdir.join("image.png"),
        [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    )
    .await
    .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(&ctx, json!({ "filePath": "image.png" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["output"], "Image read successfully");
    assert_eq!(out["metadata"]["preview"], "Image read successfully");
    assert_eq!(out["metadata"]["truncated"], false);
    assert_eq!(out["metadata"]["loaded"], json!([]));
    assert_eq!(out["attachments"][0]["type"], "file");
    assert_eq!(out["attachments"][0]["mime"], "image/png");
    assert_eq!(
        out["attachments"][0]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}

#[tokio::test]
async fn read_rejects_binary_files_before_text_decoding() {
    // Given
    let workdir = tempdir();
    let target = workdir.join("payload.bin");
    tokio::fs::write(&target, "plain text but binary extension\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let result = tool
        .execute(&ctx, json!({ "filePath": "payload.bin" }))
        .await;

    // Then
    assert!(
        matches!(result, Err(ToolError::Other(message)) if message == format!("Cannot read binary file: {}", target.to_string_lossy()))
    );
}
