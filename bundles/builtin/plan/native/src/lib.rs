//! Native implementation of the `hya/plan` built-in bundle (`plan_exit` tool
//! and interactive `tool.visibility` hook) exported via Lockstep Native ABI v2.

use std::sync::Arc;

/// Interactive `plan_exit` approval tool.
pub mod plan_exit;
/// `tool.visibility` hook hiding `plan_exit` in non-interactive runs.
pub mod visibility_hook;

pub use plan_exit::PlanExitTool;
pub use visibility_hook::{PLAN_EXIT_VISIBILITY_HOOK_ID, PlanExitVisibilityHook};

hya_runtime_abi::export_native_bundle_v2!(|out| {
    out.tools.push(Arc::new(PlanExitTool));
    out.hooks.push(Arc::new(PlanExitVisibilityHook));
});

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use hya_proto::{AgentName, SessionId, ToolCallId};
    use hya_runtime_abi::{
        HookPoint, NativeHook, ToolVisibilityInput, ToolVisibilityOutcome, abi_digest_v2,
    };
    use hya_tool::handle::ArtifactPlane;
    use hya_tool::{
        FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane, NestedToolPlane,
        PermissionPlane, PermissionRules, ProjectActivityPlane, QuestionAnswer, QuestionKind,
        SkillPlane, SpawnerPlane, Tool, ToolCtx, ToolError, ToolOperation, WorkflowPlane,
    };
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn test_ctx(session: Option<SessionId>, interaction: InteractionPlane) -> ToolCtx {
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let (spawner, _srx) = SpawnerPlane::new();
        let interaction = match session {
            Some(id) => interaction.for_session(id),
            None => interaction,
        };
        let workdir = PathBuf::from(".");
        ToolCtx {
            permission,
            interaction,
            spawner,
            workflows: WorkflowPlane::disconnected(),
            operation: ToolOperation::from_tool_call(ToolCallId::new()),
            mailbox: MailboxPlane::disconnected(),
            project_activity: ProjectActivityPlane::disconnected(),
            lifecycle: LifecyclePlane::disconnected(),
            session,
            parent_session: None,
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane::default(),
            agents: Arc::from([]),
            lsp: LspPlane::default(),
            formatter: FormatterPlane::default(),
            roots: vec![workdir.clone()],
            workdir,
            cancel: CancellationToken::new(),
            nested: NestedToolPlane::disconnected(),
        }
    }

    #[tokio::test]
    async fn plan_exit_approves_and_declines_through_interaction_plane() {
        let tool = PlanExitTool;
        assert_eq!(tool.name(), "plan_exit");
        assert_eq!(tool.schema().name.as_str(), "plan_exit");

        for (choice, expect_ok) in [(0usize, true), (1usize, false)] {
            let session = SessionId::new();
            let (interaction, mut rx) = InteractionPlane::new();
            let ctx = test_ctx(Some(session), interaction);
            let handle = tokio::spawn(async move { PlanExitTool.execute(&ctx, json!({})).await });

            let req = rx.recv().await.expect("plan_exit question request");
            assert_eq!(req.session, Some(session));
            assert_eq!(
                req.kind,
                QuestionKind::Select {
                    options: vec!["Yes".into(), "No".into()],
                    allow_custom: false,
                }
            );
            req.reply.send(QuestionAnswer::Selected(choice)).unwrap();

            let result = handle.await.unwrap();
            if expect_ok {
                let output = result.expect("approved plan_exit");
                assert_eq!(output["title"], "Switching to build agent");
                assert!(
                    output["output"]
                        .as_str()
                        .unwrap()
                        .contains("User approved switching to build agent")
                );
            } else {
                let err = result.expect_err("declined plan_exit");
                assert!(matches!(err, ToolError::Other(msg) if msg.contains("rejected")));
            }
        }
    }

    #[tokio::test]
    async fn plan_exit_requires_session_and_live_interaction_channel() {
        let (interaction, _rx) = InteractionPlane::new();
        let no_session_ctx = test_ctx(None, interaction);
        let err = PlanExitTool
            .execute(&no_session_ctx, json!({}))
            .await
            .expect_err("missing session must error");
        assert!(matches!(err, ToolError::Other(msg) if msg.contains("requires a session")));

        let (disconnected, rx) = InteractionPlane::new();
        drop(rx);
        let disconnected_ctx = test_ctx(Some(SessionId::new()), disconnected);
        let err = PlanExitTool
            .execute(&disconnected_ctx, json!({}))
            .await
            .expect_err("disconnected interaction channel must error");
        assert!(matches!(err, ToolError::Other(_)));
    }

    #[tokio::test]
    async fn visibility_hook_hides_plan_exit_only_when_non_interactive() {
        let hook = PlanExitVisibilityHook;
        assert_eq!(hook.id(), PLAN_EXIT_VISIBILITY_HOOK_ID);
        assert_eq!(hook.hook_points(), &[HookPoint::ToolVisibility]);

        let hidden = hook
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: Some(AgentName::new("hya-plan")),
                candidate_tools: vec!["read".into(), "plan_exit".into()],
                interactive: false,
            })
            .await
            .expect("non-interactive visibility outcome");
        assert_eq!(
            hidden,
            ToolVisibilityOutcome::Hide {
                tools: vec!["plan_exit".to_string()],
            }
        );

        let preserved = hook
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: Some(AgentName::new("hya-plan")),
                candidate_tools: vec!["read".into(), "plan_exit".into()],
                interactive: true,
            })
            .await
            .expect("interactive visibility outcome");
        assert_eq!(preserved, ToolVisibilityOutcome::Preserve);
    }

    #[test]
    fn abi_v2_symbol_reports_current_lockstep_digest() {
        let mut out = [0_u8; 32];
        // SAFETY: `out` is a writable 32-byte array.
        unsafe { hya_native_abi_v2(out.as_mut_ptr()) };
        assert_eq!(out, abi_digest_v2());
    }
}
