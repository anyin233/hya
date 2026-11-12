#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{CreateSession, EventBus};
use hya_proto::{AgentName, MailKind, SessionKind};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine() -> Arc<SessionEngine> {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        crate::test_support::runtime(ToolRegistry::builtins()),
        permission,
        EventBus::default(),
    ))
}

async fn root(engine: &SessionEngine) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("hya/echo"),
            workdir: ".".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn parked(supervisor: &ResidentSupervisor, root: SessionId) -> (SessionId, String) {
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("hya/echo"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = supervisor.engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (session, handle, _) = supervisor
        .spawn_resident_parked(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            "parked".into(),
            None,
            None,
        )
        .await
        .unwrap();
    (session, handle)
}

#[tokio::test]
async fn mail_revival_reserves_the_team_before_claim_registration() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let mail = tokio::spawn({
        let engine = engine.clone();
        let handle = handle.clone();
        async move {
            engine
                .mail_send(
                    root,
                    MailEndpoint::Handle(handle),
                    MailKind::Message,
                    "resume".into(),
                )
                .await
        }
    });
    entered.notified().await;
    let team = supervisor.teams().get(&root).unwrap().clone();
    drop(lead_turn);
    // Force the last-main-turn retirement at the selected-team/claim-write gap.
    supervisor.retire_team(&team);
    release.notify_one();
    let result = mail.await.unwrap();
    assert!(
        result.is_ok(),
        "revival inherited retirement cancellation: {result:?}"
    );
    assert!(supervisor.team_with_resident(child).is_some());
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn cold_mail_revival_falls_back_from_missing_root_agent() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    supervisor.teardown_root(root).await.unwrap();
    drop(lead_turn);
    engine
        .store()
        .append_event(
            root,
            &Event::AgentSwitched {
                session: root,
                agent: AgentName::new("removed-bundle-agent"),
            },
        )
        .await
        .unwrap();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "resume".into(),
        )
        .await
        .unwrap();
    assert!(supervisor.team_with_resident(child).is_some());
    assert_eq!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .agent
            .unwrap()
            .as_str(),
        crate::TASK_AGENT_ID
    );
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(5))
        .await;
}

struct StuckParams(Arc<Notify>);

#[async_trait::async_trait]
impl crate::hooks::HookDispatcher for StuckParams {
    fn dispatch_event(&self, _: &hya_proto::Envelope) {}
    async fn command_execute_before(
        &self,
        i: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(
        &self,
        i: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(
        &self,
        i: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        crate::hooks::NoopHookHost.message_user_before(i).await
    }
    async fn tool_execute_before(
        &self,
        i: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(
        &self,
        i: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::NoopHookHost.tool_execute_after(i).await
    }
    async fn chat_params(
        &self,
        _: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn drain_cause_reaches_the_member_straggler_close() {
    let entered = Arc::new(Notify::new());
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            crate::test_support::runtime(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_hooks(Arc::new(StuckParams(entered.clone()))),
    );
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = parked(&supervisor, root).await;
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "work".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    // The episode's five-second close must win, rather than the generic turn drain.
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(6))
        .await;
    let projection = engine.read_projection(child).await.unwrap();
    let answer = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == hya_proto::Role::Assistant)
        .unwrap();
    assert_eq!(answer.cause, Some(FinishCause::UserCancel));
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
}
