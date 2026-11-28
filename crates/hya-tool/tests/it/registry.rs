//! Registry invariants: canonical names, hidden aliases, bundle ownership,
//! namespacing, `wait` override precedence, output spill, and native ABI replacement.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{SessionId, ToolName, ToolSchema};
use hya_tool::handle::ArtifactStore;
use hya_tool::{
    DuplicateName, LifecyclePlane, LifecycleRequest, MAX_TOOL_OUTPUT_CHARS, Tool, ToolCtx,
    ToolError, ToolPermission, ToolRegistry, ToolResultPolicy, WaitOutcome, WaitWake,
    cap_tool_output_spilling, namespace_of, namespaced_name,
};
use serde_json::{Value, json};

use crate::common::{TempDirGuard, ctx_with, tempdir};

struct NamedTestTool(&'static str);

#[async_trait]
impl Tool for NamedTestTool {
    fn name(&self) -> &str {
        self.0
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "test".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

#[test]
fn registry_rejects_duplicates_hides_aliases_and_freezes_snapshots() {
    let registry = hya_tool_loader::builtins();
    assert!(registry.register(Arc::new(NamedTestTool("read"))).is_err());
    assert!(
        registry
            .get("read")
            .unwrap()
            .schema()
            .description
            .starts_with("Read a file or directory's contents.")
    );

    let visible: Vec<_> = registry
        .schemas()
        .into_iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    for (canonical, alias) in [
        ("webfetch", "fetch"),
        ("websearch", "search"),
        ("ask_user", "question"),
        ("apply_patch", "patch"),
        ("plan_exit", "plan"),
    ] {
        assert!(registry.get(canonical).is_some(), "{canonical} missing");
        assert!(registry.get(alias).is_some(), "{alias} missing");
        assert!(visible.iter().any(|n| n == canonical), "{canonical} hidden");
        assert!(visible.iter().all(|n| n != alias), "{alias} leaked");
    }

    let frozen = registry.snapshot();
    registry.remove("websearch");
    assert!(registry.get("websearch").is_none());
    assert!(registry.get("search").is_none());
    assert!(frozen.resolve("websearch").is_some());

    registry
        .register_with_permission(Arc::new(NamedTestTool("late_tool")), ToolPermission::Mcp)
        .unwrap();
    assert!(frozen.resolve("late_tool").is_none());
    assert_eq!(
        registry.resolve("late_tool").unwrap().permission,
        ToolPermission::Mcp
    );
}

#[test]
fn dispatch_identity_and_permission_metadata_resolution() {
    let first = hya_tool_loader::builtins().snapshot();
    let second = hya_tool_loader::builtins().snapshot();
    let canonical_names: Vec<_> = first
        .canonical_tools()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(canonical_names.len(), 23);
    for name in &canonical_names {
        let a = first.dispatch_identity_v1(name).expect("builtin identity");
        assert_eq!(Some(a), second.dispatch_identity_v1(name));
        assert_ne!(a, [0_u8; 32]);
    }

    let registry = hya_tool_loader::builtins();
    registry
        .register_with_permission(
            Arc::new(NamedTestTool("unidentified")),
            ToolPermission::Tool,
        )
        .unwrap();
    registry
        .register_with_permission_and_aliases_and_dispatch_identity(
            Arc::new(NamedTestTool("identified")),
            ToolPermission::Mcp,
            &["identified_alias".to_string()],
            [0x42_u8; 32],
        )
        .unwrap();
    let snap = registry.snapshot();
    assert_eq!(snap.dispatch_identity_v1("unidentified"), None);
    assert_eq!(snap.dispatch_identity_v1("identified"), Some([0x42_u8; 32]));
    assert_eq!(snap.dispatch_identity_v1("identified_alias"), None);
    registry.remove("identified");
    assert_eq!(registry.snapshot().dispatch_identity_v1("identified"), None);
    assert_eq!(snap.dispatch_identity_v1("identified"), Some([0x42_u8; 32]));

    for name in [
        "read",
        "ls",
        "glob",
        "find",
        "grep",
        "lsp",
        "list_agents",
        "list_channel",
        "search_agent",
    ] {
        assert_eq!(
            registry.resolve(name).unwrap().permission,
            ToolPermission::ReadOnly
        );
    }
    assert_eq!(
        registry.resolve("task").unwrap().permission,
        ToolPermission::Task
    );
    assert_eq!(
        registry.resolve("write").unwrap().permission,
        ToolPermission::Tool
    );
    let alias = registry.resolve("fetch").unwrap();
    assert_eq!(alias.tool.name(), "webfetch");
    assert_eq!(alias.permission, ToolPermission::Tool);

    registry
        .register(Arc::new(NamedTestTool("mcp__looks_like_mcp")))
        .unwrap();
    assert_eq!(
        registry.resolve("mcp__looks_like_mcp").unwrap().permission,
        ToolPermission::Tool
    );
}

#[test]
fn namespaced_names_compose_validate_and_register() {
    let full = namespaced_name("alpha", "update_status").unwrap();
    assert_eq!(full, "alpha__update_status");
    assert_eq!(namespace_of(&full), Some("alpha"));
    assert_eq!(namespace_of("read"), None);
    assert_eq!(namespace_of("mcp__github__read"), Some("mcp"));
    assert_eq!(namespace_of("__read"), None);
    assert_eq!(namespace_of("alpha__"), None);
    assert_eq!(
        namespaced_name("my_tools", "update-status").unwrap(),
        "my_tools__update-status"
    );

    for (ns, local) in [
        ("", "read"),
        ("alpha", ""),
        ("to do", "read"),
        ("alpha:", "read"),
        ("alpha", "re/ad"),
        ("alpha__x", "read"),
        ("alpha", "read__status"),
    ] {
        assert!(
            namespaced_name(ns, local).is_err(),
            "ns={ns:?} local={local:?}"
        );
    }

    let registry = hya_tool_loader::builtins();
    registry
        .register_with_permission(Arc::new(NamedTestTool("alpha__read")), ToolPermission::Tool)
        .unwrap();
    registry
        .register_with_permission(Arc::new(NamedTestTool("beta__read")), ToolPermission::Tool)
        .unwrap();
    let err = registry
        .register_with_permission(Arc::new(NamedTestTool("alpha__read")), ToolPermission::Tool)
        .unwrap_err();
    assert!(matches!(err, DuplicateName { ref name } if name == "alpha__read"));
}

#[test]
fn builtin_bundle_ownership_and_removed_kill_tool() {
    let registry = hya_tool_loader::builtins();
    for (bundle, names) in [
        ("hya/hashline", ["read", "write", "edit", "grep"].as_slice()),
        (
            "hya/coding-extras",
            [
                "ls",
                "glob",
                "find",
                "apply_patch",
                "ask_user",
                "lsp",
                "project_activity",
            ]
            .as_slice(),
        ),
        (
            "hya/extended-tools",
            ["list_agents", "task", "workflow", "search_agent", "archive"].as_slice(),
        ),
        ("hya/plan", ["plan_exit"].as_slice()),
        ("hya/network", ["webfetch", "websearch"].as_slice()),
        (
            "hya/channel-tools",
            ["send", "list_channel", "report", "wait"].as_slice(),
        ),
    ] {
        for name in names {
            assert_eq!(registry.builtin_bundle_origin(name), Some(bundle), "{name}");
        }
    }

    assert!(registry.resolve("kill").is_none());
    let archive = registry.resolve("archive").expect("archive registered");
    assert_eq!(archive.permission, ToolPermission::Task);
    let schema = serde_json::to_string(&archive.tool.schema()).unwrap_or_default();
    assert!(schema.contains("\"target\"") && schema.contains("wakes it again"));
}

async fn requested_mail_wake(registry: &ToolRegistry) -> bool {
    let tool = registry.get("wait").expect("wait registered");
    let (plane, mut rx) = LifecyclePlane::new();
    let mut ctx = ctx_with(vec![], std::path::PathBuf::from("."));
    ctx.lifecycle = plane.for_session(SessionId::new());
    let call = tokio::spawn(async move { tool.execute(&ctx, json!({"timeout_secs": 5})).await });
    let Some(LifecycleRequest::Wait { spec, reply, .. }) = rx.recv().await else {
        panic!("wait must send lifecycle wait request");
    };
    reply
        .send(Ok(WaitOutcome {
            woke_by: WaitWake::NothingToWaitFor,
            finished: Vec::new(),
            already_finished: Vec::new(),
            running: Vec::new(),
            mail: Vec::new(),
            more_mail: false,
            waited_ms: 0,
        }))
        .unwrap();
    let result = call.await.unwrap().unwrap();
    assert_eq!(result["metadata"]["woke_by"], "nothing_to_wait_for");
    spec.wake_on_mail
}

#[tokio::test]
async fn wait_override_selects_mail_aware_or_member_only_variant() {
    let full = hya_tool_loader::builtins();
    assert_eq!(
        full.builtin_bundle_origin("wait"),
        Some("hya/channel-tools")
    );
    assert_eq!(
        full.resolve("wait").unwrap().permission,
        ToolPermission::ReadOnly
    );
    assert!(
        full.resolve("wait")
            .unwrap()
            .tool
            .schema()
            .description
            .contains("mail arrives")
    );
    assert_eq!(
        full.schemas()
            .iter()
            .filter(|s| s.name.as_str() == "wait")
            .count(),
        1
    );
    assert!(requested_mail_wake(&full).await);

    let without_channels = hya_tool_loader::from_tool_families(&[
        "hya/coding-extras",
        "hya/extended-tools",
        "hya/network",
    ]);
    assert!(without_channels.get("send").is_none());
    assert_eq!(
        without_channels.builtin_bundle_origin("wait"),
        Some("hya/extended-tools")
    );
    assert!(!requested_mail_wake(&without_channels).await);

    let channel_only = hya_tool_loader::from_tool_families(&["hya/channel-tools"]);
    assert_eq!(
        channel_only.builtin_bundle_origin("wait"),
        Some("hya/channel-tools")
    );
}

#[test]
fn output_spill_bounds_default_leaves_small_and_coding_untouched() {
    let root = tempdir("spill");
    let _guard = TempDirGuard::new(&root);
    let spill_dir = root.join("artifacts");
    let store = ArtifactStore::new(&spill_dir);

    // Under cap: untouched and writes nothing.
    let small = json!({ "rows": [1, 2, 3] });
    assert_eq!(
        cap_tool_output_spilling(small.clone(), ToolResultPolicy::Default, &store, "grep"),
        small
    );
    assert!(!spill_dir.exists());

    // Coding envelope: never spills.
    let envelope = json!({
        "title": "src/main.rs",
        "output": "y".repeat(MAX_TOOL_OUTPUT_CHARS * 2),
        "metadata": {},
    });
    let capped_coding =
        cap_tool_output_spilling(envelope, ToolResultPolicy::Coding, &store, "read");
    assert!(capped_coding.get("title").is_some());
    assert!(!spill_dir.exists());

    // Oversized Default output: spills complete body and names its artifact handle.
    let body = format!(
        "HEAD_LINE\n{}\nTAIL_LINE",
        "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2)
    );
    let capped = cap_tool_output_spilling(
        Value::String(body.clone()),
        ToolResultPolicy::Default,
        &store,
        "bash",
    );
    let notice = capped.as_str().expect("oversized becomes text");
    let start = notice.find("artifact://").unwrap() + "artifact://".len();
    let rest = &notice[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let id = &rest[..end];
    assert_eq!(std::fs::read_to_string(spill_dir.join(id)).unwrap(), body);
    assert!(notice.contains("\nHEAD_LINE\n") && notice.ends_with("\nTAIL_LINE"));
    assert!(notice.chars().count() <= MAX_TOOL_OUTPUT_CHARS + 200);
}

#[cfg(unix)]
#[test]
fn replacement_child() {
    let Ok(mode) = std::env::var("HYA_REPLACEMENT_TEST") else {
        return;
    };
    if mode == "wrong" {
        let panic = std::panic::catch_unwind(|| {
            hya_tool_loader::from_tool_families(&["hya/channel-tools"])
        });
        let error = panic.err().expect("wrong ABI must be rejected");
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("different hya-tool ABI"), "{message}");
        return;
    }
    let registry = hya_tool_loader::from_tool_families(&["hya/channel-tools"]);
    let tool = registry.resolve("send").unwrap();
    let ctx = ctx_with(vec![], std::env::current_dir().unwrap());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime
        .block_on(tool.tool.execute(&ctx, json!({"nonce": 42})))
        .unwrap();
    assert_eq!(
        result,
        json!({"implementation": "replacement", "input": {"nonce": 42}})
    );
    hya_tool::deadline::install(std::time::Duration::from_secs(8));
    let result = runtime
        .block_on(tool.tool.execute(&ctx, json!({})))
        .unwrap();
    assert_eq!(result["budgetSeconds"], 8);
    assert_eq!(result["cappedSeconds"], hya_tool::deadline::MIN_TOOL_SECS);
    let _ = hya_tool_loader::from_tool_families(&["hya/channel-tools"]);
    assert_eq!(
        registry.builtin_bundle_origin("send"),
        Some("hya/channel-tools")
    );
}

#[cfg(unix)]
#[test]
fn native_family_can_be_replaced_and_wrong_abi_is_rejected() {
    let executable = std::env::current_exe().unwrap();
    let examples = executable
        .ancestors()
        .map(|directory| directory.join("examples"))
        .find(|examples| examples.is_dir())
        .expect("cargo test builds the cdylib examples");
    let root = tempdir("replacement");
    let _guard = TempDirGuard::new(&root);
    for (mode, fixture) in [("valid", "replacement_tools"), ("wrong", "wrong_abi_tools")] {
        let dir = root.join(mode);
        std::fs::create_dir_all(&dir).unwrap();
        let child = dir.join("replacement-test");
        std::fs::copy(&executable, &child).unwrap();
        let src = format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            fixture,
            std::env::consts::DLL_SUFFIX
        );
        let dst = format!(
            "{}hya_channel_tools{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        std::fs::copy(examples.join(src), dir.join(dst)).expect("cdylib example exists");
        let output = std::process::Command::new(child)
            .args(["--exact", "registry::replacement_child", "--nocapture"])
            .env("HYA_REPLACEMENT_TEST", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
