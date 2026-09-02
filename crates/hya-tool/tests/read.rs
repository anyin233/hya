//! Integration tests for `hya-tool`: read.

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

#[test]
fn read_schema_exposes_canonical_path_and_bounded_controls() {
    // Given
    let tool = ToolRegistry::builtins().get("read").unwrap();

    // When
    let schema = tool.schema();
    let properties = schema.input_schema["properties"].as_object().unwrap();

    // Then
    assert_eq!(properties.len(), 4);
    assert_eq!(properties["path"]["type"], "string");
    assert!(!properties.contains_key("filePath"));
    assert_eq!(properties["offset"]["type"], "integer");
    assert_eq!(properties["offset"]["minimum"], 1);
    assert_eq!(properties["limit"]["type"], "integer");
    assert_eq!(properties["limit"]["minimum"], 1);
    assert_eq!(properties["raw"]["type"], "boolean");
    assert_eq!(schema.input_schema["required"], json!(["path"]));
}

#[tokio::test]
async fn read_accepts_captured_equal_paths_and_zero_offset() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("captured.txt"), "first\nsecond\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "filePath": "captured.txt",
                "path": "captured.txt",
                "offset": 0,
                "limit": 2000
            }),
        )
        .await
        .expect("equal legacy and canonical paths should read");

    // Then
    assert_eq!(out["title"], "captured.txt");
    assert!(out["output"].as_str().unwrap().contains("first"));
}

#[tokio::test]
async fn read_rejects_conflicting_file_path_and_path_values() {
    // Given
    let workdir = tempdir();
    tokio::fs::write(workdir.join("legacy.txt"), "legacy\n")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("canonical.txt"), "canonical\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "filePath": "legacy.txt",
                "path": "canonical.txt"
            }),
        )
        .await;

    // Then
    assert!(
        matches!(&result, Err(ToolError::Input(message))
            if message.contains("filePath") && message.contains("path")),
        "conflicting path spellings must be a typed input error: {result:?}"
    );
}

#[tokio::test]
async fn read_resolves_one_empty_path_and_rejects_absent_empty_or_null_inputs() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("target.txt"), "content\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    for input in [
        json!({ "path": "target.txt", "filePath": "" }),
        json!({ "path": "", "filePath": "target.txt" }),
    ] {
        let result = tool.execute(&ctx, input).await.unwrap();
        assert_eq!(result["content"], "content");
    }

    for input in [
        json!({}),
        json!({ "path": "", "filePath": "" }),
        json!({ "path": "target.txt", "offset": null }),
        json!({ "path": "target.txt", "limit": null }),
        json!({ "path": "target.txt", "raw": null }),
    ] {
        let result = tool.execute(&ctx, input).await;
        assert!(
            matches!(result, Err(ToolError::Input(_))),
            "invalid Read boundary input must remain typed: {result:?}"
        );
    }
}

#[tokio::test]
async fn read_raw_invalid_utf8_and_empty_file_results_keep_bounded_facts() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("raw.txt"), "alpha\nbeta\n")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("invalid.txt"), b"alpha\xFFbeta\n")
        .await
        .unwrap();
    tokio::fs::write(workdir.join("empty.txt"), b"")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let ctx = ctx_with(workdir);

    let raw = tool
        .execute(&ctx, json!({ "path": "raw.txt", "raw": true }))
        .await
        .unwrap();
    assert_eq!(raw["output"], "alpha\nbeta");
    assert_eq!(raw["content"], "alpha\nbeta");
    assert!(!raw["output"].as_str().unwrap().contains('#'));

    let invalid = tool
        .execute(&ctx, json!({ "path": "invalid.txt" }))
        .await
        .unwrap();
    assert_eq!(invalid["content"], "alpha�beta");
    assert!(
        invalid["metadata"]["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("Invalid UTF-8"))))
    );

    let empty = tool
        .execute(&ctx, json!({ "path": "empty.txt" }))
        .await
        .unwrap();
    assert_eq!(empty["content"], "");
    assert_eq!(empty["metadata"]["display"]["totalLines"], 0);
    assert_eq!(empty["metadata"]["truncated"], false);
    assert!(
        empty["output"]
            .as_str()
            .is_some_and(|output| output.contains("File is empty"))
    );
}

#[tokio::test]
async fn read_rejects_zero_limit_as_typed_input() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("target.txt"), "content\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("read").unwrap();
    let result = tool
        .execute(
            &ctx_with(workdir),
            json!({ "path": "target.txt", "limit": 0 }),
        )
        .await;
    assert!(matches!(result, Err(ToolError::Input(_))));
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
    match result {
        Err(ToolError::Input(message)) => {
            assert!(
                message.starts_with("[E_BAD_READ]"),
                "out-of-range Read must retain the stable E_BAD_READ code: {message:?}"
            );
            assert!(
                message.contains("Offset 3") && message.contains("1 lines"),
                "out-of-range Read diagnostic must retain bounded range context: {message:?}"
            );
        }
        other => panic!("out-of-range Read must be a typed E_BAD_READ input error: {other:?}"),
    }
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

#[tokio::test]
async fn read_authorizes_lexical_external_path_before_metadata_probe() {
    // Given: the requested external path is a directory, but only its own
    // child wildcard is allowed. The lexical parent remains denied so a
    // metadata/type probe must not select the narrower allow rule first.
    let workdir = tempdir();
    let outside_root = tempdir();
    let outside_directory = outside_root.join("nested");
    tokio::fs::create_dir_all(&outside_directory).await.unwrap();
    tokio::fs::write(outside_directory.join("secret.txt"), "secret\n")
        .await
        .unwrap();
    let parent_pattern = format!("{}/*", outside_root.to_string_lossy());
    let target_pattern = format!("{}/*", outside_directory.to_string_lossy());
    let (permission, _requests) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Read, "*"),
        deny(Action::ExternalDirectory, &parent_pattern),
        allow(Action::ExternalDirectory, &target_pattern),
    ]));
    let (interaction, _interaction_rx) = InteractionPlane::new();
    let (spawner, _spawner_rx) = SpawnerPlane::new();
    let ctx = ToolCtx {
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
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir,
        cancel: CancellationToken::new(),
    };
    let tool = ToolRegistry::builtins().get("read").unwrap();

    // When
    let result = tool
        .execute(&ctx, json!({ "path": outside_directory.to_string_lossy() }))
        .await;

    // Then: the denied lexical parent wins before the directory metadata can
    // steer authorization toward the target-directory wildcard.
    assert!(
        matches!(result, Err(ToolError::Permission(_))),
        "Read must authorize an external path before metadata/type probing: {result:?}"
    );
}
