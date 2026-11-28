//! Integration tests for `hya-app`: skill tool execution and catalog precedence.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_engine::skill_catalog::{
    discover_skills, discover_skills_from_dirs, discover_skills_with_builtins,
    skill_dirs_for_workdir,
};
use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, ToolCtx, ToolError, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::support::{EnvVarGuard, env_lock};

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("hya-skill-it-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

fn ctx_with(rules: Vec<Rule>, skills: SkillPlane, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        skills,
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        artifacts: ArtifactPlane::default(),
        agents: Default::default(),
        workdir,
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[tokio::test]
async fn skill_tool_loads_content_checks_permission_and_reports_missing() {
    let root = tempdir();
    let dir = root.join("writer");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    write_skill(
        &dir,
        "writer",
        "Writes concise text",
        "Use short sentences.\n",
    );
    std::fs::write(dir.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
    let tool = hya_core::kernel::tools::skill_tool();
    let snapshot = Arc::new(discover_skills_from_dirs(&[root]));

    let out = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "writer", Mode::Allow)],
                SkillPlane::from_snapshot(Arc::clone(&snapshot)),
                PathBuf::from("."),
            ),
            json!({ "name": "writer" }),
        )
        .await
        .unwrap();
    assert_eq!(out["title"], "Loaded skill: writer");
    assert_eq!(
        out["metadata"]["dir"].as_str(),
        Some(dir.to_string_lossy().as_ref())
    );
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("<skill_content name=\"writer\">"));
    assert!(output.contains("Use short sentences."));
    assert!(output.contains("scripts/run.sh"));

    let denied = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "writer", Mode::Deny)],
                SkillPlane::from_snapshot(snapshot),
                PathBuf::from("."),
            ),
            json!({ "name": "writer" }),
        )
        .await;
    assert!(matches!(denied, Err(ToolError::Permission(_))));

    let missing = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "missing", Mode::Allow)],
                SkillPlane::default(),
                PathBuf::from("."),
            ),
            json!({ "name": "missing" }),
        )
        .await;
    assert!(matches!(missing, Err(ToolError::Other(msg)) if msg.contains("skill not found")));
}

#[tokio::test]
async fn skill_tool_honors_root_fallback_precedence_and_workdir_discovery() {
    let first_root = tempdir();
    let second_root = tempdir();
    let second_dir = second_root.join("writer");
    write_skill(
        &second_dir,
        "writer",
        "Writes concise text",
        "Second root content.\n",
    );
    let tool = hya_core::kernel::tools::skill_tool();

    // Falls back to second root when first root lacks the skill.
    let out = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "writer", Mode::Allow)],
                SkillPlane::from_snapshot(Arc::new(discover_skills_from_dirs(&[
                    first_root.clone(),
                    second_root.clone(),
                ]))),
                PathBuf::from("."),
            ),
            json!({ "name": "writer" }),
        )
        .await
        .unwrap();
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("Second root content.")
    );

    // Prefers first root when both define the same name.
    let first_dir = first_root.join("writer");
    write_skill(
        &first_dir,
        "writer",
        "Writes concise text",
        "First root content.\n",
    );
    let out = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "writer", Mode::Allow)],
                SkillPlane::from_snapshot(Arc::new(discover_skills_from_dirs(&[
                    first_root,
                    second_root,
                ]))),
                PathBuf::from("."),
            ),
            json!({ "name": "writer" }),
        )
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("First root content.") && !output.contains("Second root content."));

    // Discovered workdir skill reaches the tool with isolated HOME.
    let _lock = env_lock().await;
    let home = tempdir();
    let workdir = tempdir();
    let mut env = EnvVarGuard::new();
    env.set("HOME", &home);
    write_skill(
        &workdir.join(".hya/skills/writer"),
        "writer",
        "Writes from workspace",
        "Workspace skill body.\n",
    );
    let out = tool
        .execute(
            &ctx_with(
                vec![Rule::new(Action::Skill, "writer", Mode::Allow)],
                SkillPlane::from_snapshot(Arc::new(discover_skills_with_builtins(&workdir))),
                workdir,
            ),
            json!({ "name": "writer" }),
        )
        .await
        .unwrap();
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("Workspace skill body.")
    );
}

#[tokio::test]
async fn skill_catalog_dir_order_first_win_and_builtin_override_precedence() {
    let _lock = env_lock().await;
    let home = tempdir();
    let workdir = tempdir();
    let mut env = EnvVarGuard::new();
    env.set("HOME", &home);

    let dirs = skill_dirs_for_workdir(&workdir);
    assert_eq!(dirs[0], workdir.join(".hya/skills"));
    assert_eq!(dirs[1], home.join(".config/hya/skills"));
    assert_eq!(dirs[2], home.join(".claude/skills"));
    assert_eq!(dirs[3], workdir.join(".agents/skills"));
    assert_eq!(dirs[4], home.join(".codex/skills"));
    assert_eq!(dirs[5], home.join(".agents/skills"));
    assert!(
        !dirs
            .iter()
            .any(|d| d.to_string_lossy().contains("opencode"))
    );

    write_skill(
        &workdir.join(".hya/skills/z-local"),
        "shared",
        "Local shared",
        "local body",
    );
    write_skill(
        &workdir.join(".hya/skills/a-local"),
        "local-only",
        "Local only",
        "local only",
    );
    write_skill(
        &home.join(".config/hya/skills/shared"),
        "shared",
        "Home shared",
        "home body",
    );
    write_skill(
        &home.join(".config/hya/skills/b-home"),
        "home-only",
        "Home only",
        "home only",
    );
    let skills = discover_skills(&workdir);
    let names = skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>();
    assert_eq!(names, vec!["local-only", "shared", "home-only"]);
    let shared = skills.iter().find(|s| s.name == "shared").unwrap();
    assert_eq!(shared.description, "Local shared");
    assert_eq!(shared.content, "local body");

    // Filesystem skill overrides embedded builtin with the same name while keeping other builtins.
    write_skill(
        &workdir.join(".hya/skills/native-authoring"),
        "agent-bundle-authoring",
        "Native authoring",
        "native authoring body",
    );
    let merged = discover_skills_with_builtins(&workdir);
    let authoring = merged
        .iter()
        .filter(|s| s.name == "agent-bundle-authoring")
        .collect::<Vec<_>>();
    assert_eq!(authoring.len(), 1);
    assert_eq!(
        authoring[0].origin,
        hya_tool::SkillCatalogOrigin::Filesystem
    );
    assert_eq!(authoring[0].content, "native authoring body");
    assert!(merged.iter().any(|s| {
        s.name == "secure-self-update" && s.origin == hya_tool::SkillCatalogOrigin::Embedded
    }));
}

#[test]
fn skill_tool_lives_in_core_not_multi_agent_and_registers_in_bare_runtime() {
    assert!(
        hya_tool_loader::builtins().get("skill").is_none(),
        "skill must not remain in bundles/builtin/multi-agent"
    );
    assert!(
        hya_core::kernel::tools::bare_tools()
            .iter()
            .any(|tool| tool.name() == hya_core::kernel::tools::SKILL),
        "skill must be part of hya-bare core tools"
    );

    let bare_tools = hya_tool_loader::from_tool_families(&[]);
    let catalog = Arc::new(
        hya_engine::AgentCatalog::new(Arc::new(
            hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
        ))
        .unwrap(),
    );
    let runtime = hya_engine::RuntimeRegistry::new(bare_tools, catalog);
    let binding = runtime.bind_turn(Path::new(".")).unwrap();
    let resolved = binding
        .resolve_tool("skill")
        .expect("bare runtime must expose core skill tool");
    assert_eq!(resolved.permission, hya_tool::ToolPermission::ReadOnly);
}
