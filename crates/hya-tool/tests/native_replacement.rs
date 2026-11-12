//! Exercise discovery, ABI checking and dispatch without replacing shared build artifacts.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use hya_tool::handle::ArtifactPlane;
use hya_tool::*;
use serde_json::json;
use std::{path::PathBuf, process::Command};
use tokio_util::sync::CancellationToken;

fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
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
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        roots: vec![workdir.clone()],
        workdir,
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[test]
fn replacement_child() {
    let Ok(mode) = std::env::var("HYA_REPLACEMENT_TEST") else {
        return;
    };
    if mode == "wrong" {
        let panic =
            std::panic::catch_unwind(|| ToolRegistry::from_tool_families(&["hya/todo-tools"]));
        let error = panic.err().expect("wrong ABI must be rejected");
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(message.contains("different hya-tool ABI"), "{message}");
        return;
    }
    let registry = ToolRegistry::from_tool_families(&["hya/todo-tools"]);
    let tool = registry.resolve("todo__read").unwrap();
    let ctx = ctx_with(vec![], std::env::current_dir().unwrap());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime
        .block_on(tool.tool.execute(&ctx, json!({"nonce":42})))
        .unwrap();
    assert_eq!(
        result,
        json!({"implementation":"replacement", "input":{"nonce":42}})
    );
    assert_eq!(
        registry.builtin_bundle_origin("todo__read"),
        Some("hya/todo-tools")
    );
}

#[test]
fn native_family_can_be_replaced_and_wrong_abi_is_rejected() {
    let executable = std::env::current_exe().unwrap();
    let examples = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples");
    let root = std::env::temp_dir().join(format!("hya-replacement-{}", std::process::id()));
    for (mode, fixture) in [("valid", "replacement_tools"), ("wrong", "wrong_abi_tools")] {
        let dir = root.join(mode);
        std::fs::create_dir_all(&dir).unwrap();
        let child = dir.join("replacement-test");
        std::fs::copy(&executable, &child).unwrap();
        let filename = format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            fixture,
            std::env::consts::DLL_SUFFIX
        );
        std::fs::copy(
            examples.join(filename),
            dir.join(format!(
                "{}hya_todo_tools{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            )),
        )
        .expect("cargo test builds the cdylib examples");
        let output = Command::new(child)
            .args(["--exact", "replacement_child", "--nocapture"])
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
    std::fs::remove_dir_all(root).unwrap();
}
