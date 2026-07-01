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
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-skill-tool-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn ctx_with(rules: Vec<Rule>, skills: SkillPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills,
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn skill_loads_named_skill_content_and_file_sample() {
    // Given
    let root = tempdir();
    let dir = root.join("writer");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: writer\ndescription: Writes concise text\n---\nUse short sentences.\n",
    )
    .unwrap();
    std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    let tool = ToolRegistry::builtins().get("skill").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Skill, "writer")],
        SkillPlane::new(vec![root]),
    );

    // When
    let out = tool
        .execute(&ctx, json!({ "name": "writer" }))
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "Loaded skill: writer");
    assert_eq!(out["metadata"]["name"], "writer");
    assert_eq!(
        out["metadata"]["dir"].as_str(),
        Some(dir.to_string_lossy().as_ref())
    );
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("<skill_content name=\"writer\">"));
    assert!(output.contains("# Skill: writer"));
    assert!(output.contains("Use short sentences."));
    assert!(output.contains("<file>"));
    assert!(output.contains("scripts/run.sh"));
}

#[tokio::test]
async fn skill_requires_skill_permission_before_loading_content() {
    // Given
    let root = tempdir();
    let dir = root.join("writer");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: writer\ndescription: Writes concise text\n---\nsecret body\n",
    )
    .unwrap();
    let tool = ToolRegistry::builtins().get("skill").unwrap();
    let ctx = ctx_with(
        vec![deny(Action::Skill, "writer")],
        SkillPlane::new(vec![root]),
    );

    // When
    let result = tool.execute(&ctx, json!({ "name": "writer" })).await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
}

#[tokio::test]
async fn skill_returns_not_found_error_for_unknown_name() {
    // Given
    let tool = ToolRegistry::builtins().get("skill").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Skill, "missing")],
        SkillPlane::new(vec![]),
    );

    // When
    let result = tool.execute(&ctx, json!({ "name": "missing" })).await;

    // Then
    match result {
        Err(ToolError::Other(message)) => assert!(message.contains("skill not found")),
        other => panic!("expected missing skill error, got {other:?}"),
    }
}

#[tokio::test]
async fn skill_falls_back_to_later_root_when_first_root_has_no_match() {
    // Given
    let first_root = tempdir();
    let second_root = tempdir();
    let second_dir = second_root.join("writer");
    std::fs::create_dir_all(&second_dir).unwrap();
    std::fs::write(
        second_dir.join("SKILL.md"),
        "---\nname: writer\ndescription: Writes concise text\n---\nSecond root content.\n",
    )
    .unwrap();
    let tool = ToolRegistry::builtins().get("skill").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Skill, "writer")],
        SkillPlane::new(vec![first_root, second_root]),
    );

    // When
    let out = tool
        .execute(&ctx, json!({ "name": "writer" }))
        .await
        .unwrap();

    // Then
    let expected_dir = std::fs::canonicalize(&second_dir).unwrap();
    assert_eq!(out["metadata"]["name"], "writer");
    assert_eq!(
        out["metadata"]["dir"].as_str(),
        Some(expected_dir.to_string_lossy().as_ref())
    );
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("Second root content."));
}

#[tokio::test]
async fn skill_prefers_first_matching_root_when_multiple_roots_define_same_name() {
    // Given
    let first_root = tempdir();
    let first_dir = first_root.join("writer");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::write(
        first_dir.join("SKILL.md"),
        "---\nname: writer\ndescription: Writes concise text\n---\nFirst root content.\n",
    )
    .unwrap();

    let second_root = tempdir();
    let second_dir = second_root.join("writer");
    std::fs::create_dir_all(&second_dir).unwrap();
    std::fs::write(
        second_dir.join("SKILL.md"),
        "---\nname: writer\ndescription: Writes concise text\n---\nSecond root content.\n",
    )
    .unwrap();

    let tool = ToolRegistry::builtins().get("skill").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Skill, "writer")],
        SkillPlane::new(vec![first_root, second_root]),
    );

    // When
    let out = tool
        .execute(&ctx, json!({ "name": "writer" }))
        .await
        .unwrap();

    // Then
    let expected_dir = std::fs::canonicalize(&first_dir).unwrap();
    assert_eq!(
        out["metadata"]["dir"].as_str(),
        Some(expected_dir.to_string_lossy().as_ref())
    );
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("First root content."));
    assert!(!output.contains("Second root content."));
}
