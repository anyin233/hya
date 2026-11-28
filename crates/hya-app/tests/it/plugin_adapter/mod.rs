//! Integration tests for `hya-app`'s `PluginHostAdapter` and `PluginHost`
//! wire dispatch, evaluator chains, bundle capabilities, and process lifecycle.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bundle_tools_and_apis;
mod evaluators_and_policy;
mod hooks_dispatch;

use std::collections::BTreeMap;
use std::path::PathBuf;

use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{
    AgentSpawnParams, CompactionAfterParams, CompactionBeforeOutcomeWire, CompactionBeforeParams,
    CompactionTriggerWire, HookName, HostInfo, ModelFallbackOutcomeWire, ModelFallbackParams,
    PermissionApproveParams, PermissionOutcomeWire, PluginKindWire, SessionLifecycleParams,
    WireResource,
};
use hya_proto::{ModelRef, SessionId};
use hya_tool::{
    Action, FormatterPlane, InteractionPlane, LspPlane, PermissionPlane, PermissionRules,
    SkillPlane, SpawnerPlane, ToolCtx, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub(super) fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

pub(super) fn py_spec(id: &str, script: &str) -> PluginSpec {
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: vec!["python3".to_string(), "-c".to_string(), script.to_string()],
        timeout_ms: Some(4000),
        env: BTreeMap::new(),
        plugin_dir: None,
        launch: Default::default(),
    }
}

pub(super) fn wire_request(model: &str) -> hya_provider::CompletionRequest {
    hya_provider::CompletionRequest {
        model: ModelRef::new(model),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: BTreeMap::new(),
    }
}

pub(super) fn ctx_with(session: SessionId, workdir: &str) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from(workdir),
        roots: vec![PathBuf::from(workdir)],
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

/// Wire hook names and documented DTO shapes round-trip through serde without
/// spawning any external plugin process.
#[test]
fn wire_hook_names_and_shapes_round_trip() {
    for (name, wire) in [
        (HookName::CompactionBefore, "compaction.before"),
        (HookName::CompactionAfter, "compaction.after"),
        (HookName::SessionStart, "session.start"),
        (HookName::SessionEnd, "session.end"),
        (HookName::AgentSpawn, "agent.spawn"),
        (HookName::GoalEvaluate, "goal.evaluate"),
        (HookName::LoopVerifier, "loop.verifier"),
        (HookName::LoopPlanner, "loop.planner"),
        (HookName::LoopShouldStop, "loop.should_stop"),
        (HookName::ModelFallback, "model.fallback"),
        (HookName::PermissionApprove, "permission.approve"),
    ] {
        assert_eq!(serde_json::to_string(&name).unwrap(), format!("\"{wire}\""));
        assert_eq!(HookName::from_wire(wire), Some(name));
        assert_eq!(name.as_str(), wire);
        assert_eq!(name.method(), format!("hook/{wire}"));
    }
    assert_eq!(HookName::from_wire("compaction.beforex"), None);

    let session = SessionId::new();
    let before = CompactionBeforeParams {
        session,
        trigger: CompactionTriggerWire::Overflow,
        messages_token_estimate: 4_200,
    };
    let val = serde_json::to_value(&before).unwrap();
    assert_eq!(val["trigger"], json!("overflow"));
    assert_eq!(val["messagesTokenEstimate"], json!(4200));
    assert_eq!(
        serde_json::from_value::<CompactionBeforeParams>(val).unwrap(),
        before
    );
    let outcome = CompactionBeforeOutcomeWire::Replace {
        instructions: "do it differently".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap()["outcome"],
        "replace"
    );

    let after = CompactionAfterParams {
        session,
        summary_tokens: 128,
    };
    assert_eq!(serde_json::to_value(&after).unwrap()["summaryTokens"], 128);
    assert_eq!(
        serde_json::to_value(SessionLifecycleParams { session }).unwrap()["session"],
        serde_json::to_value(session).unwrap()
    );
    let spawn = AgentSpawnParams {
        parent: session,
        child: SessionId::new(),
    };
    let spawn_val = serde_json::to_value(&spawn).unwrap();
    assert_eq!(
        spawn_val["parent"],
        serde_json::to_value(spawn.parent).unwrap()
    );
    assert_eq!(
        spawn_val["child"],
        serde_json::to_value(spawn.child).unwrap()
    );

    // Legacy chat.params without additive lineage fields still decodes.
    let legacy: hya_plugin::messages::ChatParamsParams = serde_json::from_value(json!({
        "session": session,
        "message": hya_proto::MessageId::new(),
        "request": {"model": "fake/model", "messages": [], "tools": [], "headers": {}},
    }))
    .unwrap();
    assert_eq!(legacy.root_session, None);
    assert_eq!(legacy.agent, None);

    let fallback: ModelFallbackParams = serde_json::from_value(json!({
        "session": session,
        "root_session": session,
        "message": hya_proto::MessageId::new(),
        "model": "a/b",
        "error": {"class": "unknown_model", "message": "unknown provider for model: a/b"},
        "attempt": 1,
        "tried": ["a/b"],
    }))
    .unwrap();
    assert_eq!(fallback.agent, None);
    assert_eq!(
        serde_json::from_value::<ModelFallbackOutcomeWire>(
            json!({"outcome": "retry", "model": "c/d"})
        )
        .unwrap(),
        ModelFallbackOutcomeWire::Retry {
            model: ModelRef::new("c/d")
        }
    );
    assert_eq!(
        serde_json::from_value::<ModelFallbackOutcomeWire>(json!({"outcome": "give_up"})).unwrap(),
        ModelFallbackOutcomeWire::GiveUp
    );

    let perm = PermissionApproveParams {
        session,
        root_session: session,
        agent: None,
        mode: "careful".to_string(),
        action: Action::Bash,
        resource: WireResource::Command {
            value: "git status".to_string(),
        },
    };
    let perm_val = serde_json::to_value(&perm).unwrap();
    assert_eq!(perm_val["mode"], "careful");
    assert_eq!(perm_val["action"], "bash");
    assert_eq!(
        perm_val["resource"],
        json!({"type": "command", "value": "git status"})
    );
    assert!(perm_val.get("agent").is_none());
    assert_eq!(
        serde_json::from_value::<PermissionApproveParams>(perm_val).unwrap(),
        perm
    );
    assert_eq!(
        serde_json::from_value::<PermissionOutcomeWire>(json!({"outcome": "allow_always"}))
            .unwrap(),
        PermissionOutcomeWire::AllowAlways
    );
}
