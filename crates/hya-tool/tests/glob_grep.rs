//! `glob` and `grep` matching and workdir scoping.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const MAX_GLOB_BYTES: usize = 4096;

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
    let dir =
        std::env::temp_dir().join(format!("hya-glob-grep-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        allow(Action::Glob, "*"),
        allow(Action::Grep, "*"),
        allow(Action::ExternalDirectory, "*"),
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

fn ctx_with_rules(workdir: PathBuf, rules: Vec<Rule>) -> ToolCtx {
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

#[tokio::test]
async fn glob_supports_path_and_open_code_output_shape() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("readme.md"), "# docs\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    let ctx = ctx_with(workdir.clone());

    let out = tool
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src" }))
        .await
        .unwrap();

    let file = src.join("main.rs").to_string_lossy().replace('\\', "/");
    assert_eq!(out["title"], "src");
    assert_eq!(out["metadata"]["count"], 1);
    assert_eq!(out["metadata"]["truncated"], false);
    assert_eq!(out["output"], file);
}

#[tokio::test]
async fn glob_rejects_file_path_like_compat() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("lib.rs"), "pub fn lib() {}\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    let ctx = ctx_with(workdir.clone());

    let err = tool
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src/main.rs" }))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("glob path must be a directory"));
}

#[tokio::test]
async fn grep_supports_regex_glob_and_output_shape() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "fn main() {}\nlet x = 1;\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("notes.txt"), "fn main text file\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let out = tool
        .execute(
            &ctx,
            json!({ "pattern": "fn\\s+main", "path": "src", "glob": "*.rs" }),
        )
        .await
        .unwrap();

    assert_eq!(out["title"], "fn\\s+main");
    assert_eq!(out["metadata"]["matches"], 1);
    assert_eq!(out["metadata"]["files"], 1);
    assert_eq!(out["metadata"]["truncated"], false);
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("main.rs"));
    assert!(output.contains("fn main() {}"));
    assert!(!output.contains("notes.txt"));
}

#[tokio::test]
async fn grep_file_target_searches_only_the_requested_file() {
    let workdir = tempdir();
    let src = workdir.join("src");
    tokio::fs::create_dir_all(&src).await.unwrap();
    tokio::fs::write(src.join("main.rs"), "needle in main\n")
        .await
        .unwrap();
    tokio::fs::write(src.join("lib.rs"), "needle in lib\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let out = tool
        .execute(&ctx, json!({ "pattern": "needle", "path": "src/main.rs" }))
        .await
        .unwrap();

    assert_eq!(out["metadata"]["matches"], 1);
    assert_eq!(out["metadata"]["files"], 1);
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("main.rs"));
    assert!(!output.contains("lib.rs"));
}

#[test]
fn grep_schema_exposes_pinned_fields_and_bounds() {
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let schema = tool.schema().input_schema;
    let properties = schema["properties"].as_object().unwrap();

    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["pattern"]));
    let names = properties
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names,
        [
            "context",
            "glob",
            "ignoreCase",
            "limit",
            "literal",
            "path",
            "pattern"
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
    for name in ["pattern", "path", "glob"] {
        assert_eq!(properties[name]["type"], "string");
    }
    for name in ["ignoreCase", "literal"] {
        assert_eq!(properties[name]["type"], "boolean");
    }
    assert_eq!(properties["context"]["type"], "integer");
    assert_eq!(properties["context"]["minimum"], 0);
    assert_eq!(properties["context"]["maximum"], 5);
    assert_eq!(properties["limit"]["type"], "integer");
    assert_eq!(properties["limit"]["minimum"], 1);
    assert_eq!(properties["limit"]["maximum"], 200);
}

#[tokio::test]
async fn grep_supports_literal_case_insensitive_matching() {
    let workdir = tempdir();
    tokio::fs::write(workdir.join("notes.txt"), "needle.*\nNEEDLE.*\nneedleX\n")
        .await
        .unwrap();
    let tool = ToolRegistry::builtins().get("grep").unwrap();
    let ctx = ctx_with(workdir);

    let out = tool
        .execute(
            &ctx,
            json!({
                "pattern": "needle.*",
                "path": "notes.txt",
                "literal": true,
                "ignoreCase": true
            }),
        )
        .await
        .unwrap();

    assert_eq!(out["metadata"]["matches"], 2);
    assert_eq!(out["metadata"]["files"], 1);
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("needle.*"));
    assert!(output.contains("NEEDLE.*"));
    assert!(!output.contains("needleX"));
}

#[tokio::test]
async fn glob_requires_external_directory_permission_for_outside_path() {
    let workdir = tempdir();
    let outside = tempdir();
    tokio::fs::write(outside.join("main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    let ctx = ctx_with_rules(
        workdir,
        vec![
            allow(Action::Glob, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
    );
    let tool = ToolRegistry::builtins().get("glob").unwrap();

    let err = tool
        .execute(
            &ctx,
            json!({ "pattern": "*.rs", "path": outside.to_string_lossy() }),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, ToolError::Permission(_)));
}

/// Verify the public Glob caller accepts both standard negated classes.
#[tokio::test]
async fn glob_supports_negated_character_classes() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        for name in ["a.txt", "b.txt", "c.txt"] {
            tokio::fs::write(workdir.join(name), "content\n")
                .await
                .unwrap();
        }
        let tool = ToolRegistry::builtins().get("glob").unwrap();
        let ctx = ctx_with(workdir);

        for pattern in ["[!a].txt", "[^a].txt"] {
            let result = tool
                .execute(&ctx, json!({ "pattern": pattern }))
                .await
                .unwrap();
            assert_eq!(result["metadata"]["count"], 2, "pattern {pattern:?}");
            let paths = result["paths"]
                .as_array()
                .expect("Glob paths must be an array");
            assert_eq!(paths.len(), 2, "pattern {pattern:?}");
            assert!(
                paths
                    .iter()
                    .all(|path| { path.as_str().is_some_and(|path| !path.ends_with("a.txt")) })
            );
            assert!(
                paths
                    .iter()
                    .any(|path| { path.as_str().is_some_and(|path| path.ends_with("b.txt")) })
            );
            assert!(
                paths
                    .iter()
                    .any(|path| { path.as_str().is_some_and(|path| path.ends_with("c.txt")) })
            );
        }
    })
    .await
    .expect("negated-class Glob cases must finish within the outer timeout");
}

/// Verify a caller-provided Glob pattern is rejected before unbounded matching.
#[tokio::test]
async fn glob_rejects_over_budget_pattern() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        tokio::fs::write(workdir.join("target.txt"), "content\n")
            .await
            .unwrap();
        let pattern = "x".repeat(MAX_GLOB_BYTES + 1);
        let tool = ToolRegistry::builtins().get("glob").unwrap();
        let error = tool
            .execute(&ctx_with(workdir), json!({ "pattern": pattern }))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Input(message)
                if (message.contains("glob") || message.contains("pattern"))
                    && message.contains("4096")),
            "oversized Glob pattern must be a bounded input error: {error:?}"
        );
    })
    .await
    .expect("oversized Glob validation must finish within the outer timeout");
}

/// Verify a cancelled Glob call does not begin unbounded directory work.
#[tokio::test]
async fn glob_cancellation_stops_directory_work() {
    tokio::time::timeout(Duration::from_secs(2), async {
        let workdir = tempdir();
        tokio::fs::create_dir_all(workdir.join("nested/deeper"))
            .await
            .unwrap();
        tokio::fs::write(workdir.join("nested/deeper/target.txt"), "content\n")
            .await
            .unwrap();
        let tool = ToolRegistry::builtins().get("glob").unwrap();
        let ctx = ctx_with(workdir);
        ctx.cancel.cancel();

        let result = tool.execute(&ctx, json!({ "pattern": "**/*.txt" })).await;
        assert!(matches!(result, Err(ToolError::Cancelled)));
    })
    .await
    .expect("cancelled directory work must finish within the outer timeout");
}

/// Glob authorizes the lexical external parent before target metadata can select a resource.
#[tokio::test]
async fn glob_authorizes_external_path_before_metadata_probe() {
    let workdir = tempdir();
    let outside_root = tempdir();
    let outside_directory = outside_root.join("nested");
    tokio::fs::create_dir_all(&outside_directory).await.unwrap();
    tokio::fs::write(outside_directory.join("secret.txt"), "secret\n")
        .await
        .unwrap();
    let parent_pattern = format!("{}/*", outside_root.to_string_lossy());
    let target_pattern = format!("{}/*", outside_directory.to_string_lossy());
    let ctx = ctx_with_rules(
        workdir,
        vec![
            allow(Action::Glob, "*"),
            deny(Action::ExternalDirectory, &parent_pattern),
            allow(Action::ExternalDirectory, &target_pattern),
        ],
    );
    let tool = ToolRegistry::builtins().get("glob").unwrap();

    let result = tool
        .execute(
            &ctx,
            json!({ "pattern": "*.txt", "path": outside_directory }),
        )
        .await;

    assert!(
        matches!(result, Err(ToolError::Permission(_))),
        "Glob must not probe external target kind before lexical authorization: {result:?}"
    );
}

/// Exactly one result beyond the public cap sets truncation without retaining all paths.
#[tokio::test]
async fn glob_limit_boundary_distinguishes_exact_cap_from_cap_plus_one() {
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    for (count, expected_truncated) in [(100usize, false), (101usize, true)] {
        let workdir = tempdir();
        for index in 0..count {
            tokio::fs::write(workdir.join(format!("file-{index:03}.txt")), "content\n")
                .await
                .unwrap();
        }
        let result = tool
            .execute(&ctx_with(workdir), json!({ "pattern": "*.txt" }))
            .await
            .unwrap();

        assert_eq!(result["total"], count);
        assert_eq!(result["metadata"]["count"], count.min(100));
        assert_eq!(result["metadata"]["truncated"], expected_truncated);
        assert_eq!(result["paths"].as_array().unwrap().len(), count.min(100));
    }
}

/// Runtime Glob input stays closed and rejects null optional fields before traversal.
#[tokio::test]
async fn glob_rejects_unknown_and_null_fields() {
    let workdir = tempdir();
    let tool = ToolRegistry::builtins().get("glob").unwrap();
    let ctx = ctx_with(workdir);

    for input in [
        json!({ "pattern": "*.rs", "unknown": true }),
        json!({ "pattern": "*.rs", "path": null }),
    ] {
        let result = tool.execute(&ctx, input).await;
        assert!(
            matches!(result, Err(ToolError::Input(_))),
            "closed Glob input must reject invalid fields: {result:?}"
        );
    }
}
