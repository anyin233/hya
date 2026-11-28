//! Integration tests verifying `hya/hashline` overriding `hya-bare`'s standard
//! `read`/`write`/`edit`/`grep` by default, and clean fallback to `hya-bare`'s
//! standard tools when `bundles.builtin.disable: [hya/hashline]` is configured.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hya_app::compose_builtin_tool_registry;
use hya_bundle::{BuiltinBundleProfile, BuiltinBundleSelection};
use hya_proto::ToolCallId;
use hya_tool::handle::ArtifactPlane;
use hya_tool::{
    Action, FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane, Mode,
    NestedToolPlane, PermissionPlane, PermissionRules, ProjectActivityPlane, Rule, SkillPlane,
    SpawnerPlane, ToolCtx, ToolOperation, WebSearchPlane, WorkflowPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-app-hashline-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tool_ctx(workdir: &Path) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Edit, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]));
    ToolCtx {
        permission,
        interaction: InteractionPlane::new().0,
        spawner: SpawnerPlane::new().0,
        workflows: WorkflowPlane::disconnected(),
        operation: ToolOperation::from_tool_call(ToolCallId::new()),
        mailbox: MailboxPlane::disconnected(),
        project_activity: ProjectActivityPlane::disconnected(),
        lifecycle: LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        agents: Arc::from([]),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: FormatterPlane::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: CancellationToken::new(),
        nested: NestedToolPlane::disconnected(),
    }
}

#[tokio::test]
async fn toggling_bundles_builtin_disable_hashline_switches_between_hashline_and_hya_bare_standard()
{
    let dir = ScratchDir::new("toggle");
    let ctx = tool_ctx(&dir.0);
    let sample = dir.0.join("sample.txt");
    std::fs::write(&sample, "first line\nsecond line\nthird line\n").unwrap();

    // 1. Default (`profile: full`): `hya/hashline` loads via ABI v2 and overrides `hya/bare`.
    let full_selection = BuiltinBundleSelection::full();
    assert!(full_selection.is_enabled("hya/hashline"));
    let full_registry =
        compose_builtin_tool_registry(&full_selection).expect("compose full registry");

    for name in ["read", "write", "edit", "grep"] {
        assert_eq!(
            full_registry.builtin_bundle_origin(name),
            Some("hya/hashline"),
            "{name} should come from hya/hashline under profile: full"
        );
    }
    assert_eq!(
        full_registry.builtin_bundle_origin("ls"),
        Some("hya/base-tools")
    );
    // `todo` and `rewrite` remain permanently kernel-exclusive.
    assert!(full_registry.get("todo").is_none());
    assert!(full_registry.get("rewrite").is_none());

    // Hashline `read` emits LINE#HASH anchors inside `<content>`.
    let hashline_read = full_registry
        .get("read")
        .unwrap()
        .execute(&ctx, json!({ "path": "sample.txt" }))
        .await
        .unwrap();
    let hashline_read_out = hashline_read["output"].as_str().unwrap();
    assert!(
        hashline_read_out.contains("<content>\n") && hashline_read_out.contains("2#"),
        "expected hashline output, got: {hashline_read_out}"
    );
    let line2_anchor = hashline_read_out
        .lines()
        .find(|line| line.starts_with("2#"))
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .to_string();

    // Hashline `edit` uses `edits: [{ op: "replace", pos: "LINE#HASH", ... }]`.
    let hashline_edit = full_registry
        .get("edit")
        .unwrap()
        .execute(
            &ctx,
            json!({
                "path": "sample.txt",
                "edits": [{ "op": "replace", "pos": line2_anchor, "lines": ["second via hashline"] }]
            }),
        )
        .await
        .unwrap();
    assert_eq!(hashline_edit["metadata"]["classification"], "applied");

    // Hashline `grep` returns anchored matches.
    let hashline_grep = full_registry
        .get("grep")
        .unwrap()
        .execute(
            &ctx,
            json!({ "pattern": "via hashline", "path": "sample.txt" }),
        )
        .await
        .unwrap();
    let hashline_grep_out = hashline_grep["output"].as_str().unwrap();
    assert!(
        hashline_grep_out.contains("2#") && hashline_grep_out.contains("second via hashline"),
        "expected hashline grep output, got: {hashline_grep_out}"
    );

    // 2. Config with `bundles.builtin.disable: [hya/hashline]` skips `hya/hashline`
    //    and falls back cleanly to `hya-bare`'s standard `read`/`write`/`edit`/`grep`.
    let yaml = r#"
schema_version: 2
bundles:
  builtin:
    profile: full
    disable:
      - hya/hashline
"#;
    #[derive(serde::Deserialize)]
    struct Wrapper {
        bundles: BundlesBlock,
    }
    #[derive(serde::Deserialize)]
    struct BundlesBlock {
        builtin: BuiltinBundleSelection,
    }
    let parsed: Wrapper = serde_norway::from_str(yaml).expect("parse v2 config.yaml snippet");
    let disabled_selection = parsed.bundles.builtin;
    assert_eq!(disabled_selection.profile, BuiltinBundleProfile::Full);
    assert!(!disabled_selection.is_enabled("hya/hashline"));
    assert!(disabled_selection.unknown_identities().is_empty());

    let fallback_registry =
        compose_builtin_tool_registry(&disabled_selection).expect("compose fallback registry");

    for name in ["read", "write", "edit", "grep"] {
        assert_eq!(
            fallback_registry.builtin_bundle_origin(name),
            Some("hya/bare"),
            "{name} should fall back to hya/bare when hya/hashline is disabled"
        );
    }
    assert_eq!(
        fallback_registry.builtin_bundle_origin("ls"),
        Some("hya/base-tools")
    );
    assert!(fallback_registry.get("todo").is_none());
    assert!(fallback_registry.get("rewrite").is_none());

    // Standard `read` returns verbatim unanchored file contents (no `<content>` / `2#` hashes).
    let std_read = fallback_registry
        .get("read")
        .unwrap()
        .execute(&ctx, json!({ "path": "sample.txt" }))
        .await
        .unwrap();
    assert_eq!(
        std_read["output"],
        "first line\nsecond via hashline\nthird line\n"
    );

    // Standard `edit` accepts `{ path, old, new }`.
    let std_edit = fallback_registry
        .get("edit")
        .unwrap()
        .execute(
            &ctx,
            json!({
                "path": "sample.txt",
                "old": "second via hashline",
                "new": "second via standard"
            }),
        )
        .await
        .unwrap();
    assert_eq!(std_edit["replaced"], 1);
    assert_eq!(
        std::fs::read_to_string(&sample).unwrap(),
        "first line\nsecond via standard\nthird line\n"
    );

    // Standard `grep` returns `2:text` with zero hashline anchors.
    let std_grep = fallback_registry
        .get("grep")
        .unwrap()
        .execute(
            &ctx,
            json!({ "pattern": "via standard", "path": "sample.txt" }),
        )
        .await
        .unwrap();
    let std_grep_out = std_grep["output"].as_str().unwrap();
    assert!(
        std_grep_out.contains("sample.txt:\n2:second via standard"),
        "expected standard grep output, got: {std_grep_out}"
    );
    assert!(
        !std_grep_out.contains("2#"),
        "standard grep must not emit hashes"
    );

    // Standard `write` creates a new file cleanly.
    let std_write = fallback_registry
        .get("write")
        .unwrap()
        .execute(
            &ctx,
            json!({ "path": "created.txt", "content": "plain write\n" }),
        )
        .await
        .unwrap();
    assert_eq!(std_write["ok"], true);
    assert_eq!(
        std::fs::read_to_string(dir.0.join("created.txt")).unwrap(),
        "plain write\n"
    );

    // 3. `profile: bare` loads zero built-in bundles and exposes all non-reserved `hya/bare` tools.
    let bare_registry =
        compose_builtin_tool_registry(&BuiltinBundleSelection::bare()).expect("compose bare");
    for name in ["read", "write", "edit", "bash", "grep", "skill"] {
        assert_eq!(
            bare_registry.builtin_bundle_origin(name),
            Some("hya/bare"),
            "{name} should come from hya/bare under profile: bare"
        );
    }
    assert!(bare_registry.get("ls").is_none());
    assert!(bare_registry.get("webfetch").is_none());
    assert!(bare_registry.get("todo").is_none());
    assert!(bare_registry.get("rewrite").is_none());
}
