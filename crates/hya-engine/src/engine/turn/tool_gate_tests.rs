#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_proto::{AgentName, Event, ModelRef, SessionKind, ToolName, ToolSchema};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_runtime_abi::{
    HookPoint, HostError, HostRuntime, NativeHook, NativeToolAfterInput, NativeToolAfterOutcome,
};
use hya_tool::{PermissionPlane, PermissionRules, Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::append_steer_notice;
use crate::engine::{AgentSpec, SessionEngine};
use crate::{CreateSession, EventBus};

const BUNDLE: &str = "test/staged";

/// Succeeds unless its input says `{"fail": true}`.
struct ProbeTool;

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        "probe"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("probe"),
            description: "test probe".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }
    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if input["fail"] == true {
            return Err(ToolError::Other("probe failed".into()));
        }
        Ok(json!({"output": "probed"}))
    }
}

/// Stages one event per tool result; its callback records whether the event
/// was already published, i.e. committed.
struct StagingHook {
    host: Arc<dyn HostRuntime>,
    bus: EventBus,
    callbacks: Arc<Mutex<Vec<bool>>>,
}

#[async_trait]
impl NativeHook for StagingHook {
    fn id(&self) -> &str {
        "staging"
    }
    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolExecuteAfter]
    }
    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        let mut published = self.bus.subscribe();
        let callbacks = Arc::clone(&self.callbacks);
        self.host.session_store().stage_extension_event(
            input.session,
            input.session,
            BUNDLE,
            "staged",
            json!({"call": input.call}),
            Some(Box::new(move || {
                let seen = std::iter::from_fn(|| published.try_recv().ok()).any(
                    |env| matches!(&env.event, Event::Extension { bundle, .. } if bundle == BUNDLE),
                );
                callbacks.lock().unwrap().push(seen);
            })),
        )?;
        Ok(NativeToolAfterOutcome::Continue {
            result: input.result,
        })
    }
}

async fn engine(provider: FakeProvider, rules: PermissionRules) -> SessionEngine {
    let tools = hya_tool_loader::builtins();
    tools.register(Arc::new(ProbeTool)).unwrap();
    SessionEngine::new(
        hya_store::SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        crate::test_support::runtime(tools),
        PermissionPlane::new(rules).0,
        EventBus::default(),
    )
}

async fn root_session(engine: &SessionEngine, agent: &str) -> (hya_proto::SessionId, AgentSpec) {
    let agent = AgentSpec {
        name: AgentName::new(agent),
        model: ModelRef::new("fake"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: ".".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();
    (session, agent)
}

#[tokio::test]
async fn staged_extension_events_commit_with_the_tool_result_and_run_callbacks_after_commit() {
    let probe = |fail: bool| {
        vec![FakeStep::ToolCall {
            name: "probe".into(),
            input: json!({"fail": fail}),
        }]
    };
    let provider = FakeProvider::scripted_turns(vec![
        probe(false),
        probe(true),
        vec![FakeStep::Text("done".into())],
    ]);
    let engine = engine(provider, PermissionRules::default()).await;
    let callbacks = Arc::new(Mutex::new(Vec::new()));
    let engine = engine.clone().with_hooks(Arc::new(StagingHook {
        host: engine.channel_host_runtime(),
        bus: engine.bus().clone(),
        callbacks: Arc::clone(&callbacks),
    }));
    let (session, agent) = root_session(&engine, "build").await;
    engine
        .admit_user_prompt(session, "go".into())
        .await
        .unwrap();
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    let log = engine.store().replay(session).await.unwrap();
    let staged: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(
            |(_, env)| matches!(&env.event, Event::Extension { bundle, .. } if bundle == BUNDLE),
        )
        .map(|(index, _)| index)
        .collect();
    // Only the successful call's event commits; the failed call drops its
    // event and callback.
    assert_eq!(staged.len(), 1, "{log:#?}");
    assert!(
        matches!(&log[staged[0] - 1].event, Event::ToolResult { .. }),
        "staged event must follow its tool result in the same batch: {log:#?}"
    );
    assert_eq!(*callbacks.lock().unwrap(), vec![true]);
}

#[test]
fn steer_notices_on_one_result_accumulate() {
    for (mut output, key, expected) in [
        (
            serde_json::json!({"output":"ok"}),
            "output",
            "ok\n\nmail\n\ntime",
        ),
        (serde_json::json!({"title":"t"}), "steer", "mail\n\ntime"),
        (serde_json::json!({"output":42}), "steer", "mail\n\ntime"),
        (serde_json::json!("ok"), "steer", "mail\n\ntime"),
    ] {
        append_steer_notice(&mut output, "\n\nmail");
        append_steer_notice(&mut output, "\n\ntime");
        assert_eq!(output[key], expected);
    }
}
