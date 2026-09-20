//! Long MCP tool calls move to the background: the turn gets an early
//! "backgrounded" tool result, and completion carries the real result back as
//! a steered user prompt.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef, Role};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct SlowMcpTool {
    sleep: Duration,
}

#[async_trait]
impl Tool for SlowMcpTool {
    fn name(&self) -> &str {
        "mcp__slow__slow"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new("mcp__slow__slow"),
            description: "Sleep then return".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        tokio::time::sleep(self.sleep).await;
        Ok(json!({
            "title": "",
            "output": "slow-done",
            "metadata": {}
        }))
    }
}

async fn engine_with_background(provider: FakeProvider, budget: Duration) -> Arc<SessionEngine> {
    let tools = Arc::new(ToolRegistry::builtins());
    let tool = Arc::new(SlowMcpTool {
        sleep: Duration::from_millis(400),
    });
    tools
        .register_with_permission(tool, ToolPermission::Mcp)
        .unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Mcp,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_mcp_background_after(budget),
    )
}

fn agent() -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    }
}

async fn new_session(engine: &SessionEngine) -> hya_proto::SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap()
}

async fn user_texts(engine: &SessionEngine, session: hya_proto::SessionId) -> Vec<String> {
    let projection = engine.read_projection(session).await.unwrap();
    projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| {
            let mut text = String::new();
            for part in &m.parts {
                if let hya_proto::PartProjection::Text { text: t, .. } = part {
                    text.push_str(t);
                }
            }
            (!text.is_empty()).then_some(text)
        })
        .collect()
}

#[tokio::test]
async fn long_mcp_call_backgrounds_and_steers_completion() {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "mcp__slow__slow".to_string(),
                input: json!({}),
            },
            FakeStep::Text("meanwhile".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("reclaimed".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let engine = engine_with_background(provider, Duration::from_millis(50)).await;
    let session = new_session(&engine).await;

    let finish = engine
        .run_turn(session, &agent(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    // The tool part completes immediately with the backgrounded marker: the
    // agent learns the call moved to the background without blocking.
    let projection = engine.read_projection(session).await.unwrap();
    let tool_text = projection
        .session
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .find_map(|p| match p {
            hya_proto::PartProjection::Tool {
                name,
                state: hya_proto::ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "mcp__slow__slow" => Some(output.to_string()),
            _ => None,
        })
        .expect("tool part completed");
    assert!(
        tool_text.contains("backgrounded") && tool_text.contains("mcpbg-"),
        "early result must tell the agent the call is backgrounded: {tool_text}"
    );

    // The watcher delivers the real result as a steered user prompt.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let prompts = user_texts(&engine, session).await;
        if prompts
            .iter()
            .any(|text| text.contains("slow-done") && text.contains("mcpbg-"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background completion prompt never arrived; prompts={prompts:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let prompts = user_texts(&engine, session).await;
    let reclaim = prompts
        .iter()
        .find(|text| text.contains("slow-done"))
        .unwrap();
    assert!(
        reclaim.to_lowercase().contains("reclaim"),
        "completion prompt must steer the agent to reclaim the result: {reclaim}"
    );
}

#[tokio::test]
async fn fast_mcp_call_stays_in_foreground() {
    // A budget the 400ms tool exceeds, vs a tool replaced to be instant: the
    // no-background default must keep the ordinary synchronous path intact.
    let tools = Arc::new(ToolRegistry::builtins());
    let tool = Arc::new(SlowMcpTool {
        sleep: Duration::ZERO,
    });
    tools
        .register_with_permission(tool, ToolPermission::Mcp)
        .unwrap();
    let router = Arc::new(
        ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
            FakeStep::ToolCall {
                name: "mcp__slow__slow".to_string(),
                input: json!({}),
            },
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]]))),
    );
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Mcp,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    ));
    let session = new_session(&engine).await;
    engine
        .run_turn(session, &agent(), CancellationToken::new())
        .await
        .unwrap();

    let prompts = user_texts(&engine, session).await;
    assert!(
        !prompts.iter().any(|text| text.contains("slow-done")),
        "no background prompt without a budget configured"
    );
    let projection = engine.read_projection(session).await.unwrap();
    let tool_text = projection
        .session
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .find_map(|p| match p {
            hya_proto::PartProjection::Tool {
                state: hya_proto::ToolPartState::Completed { output, .. },
                ..
            } => Some(output.to_string()),
            _ => None,
        })
        .expect("tool completed");
    assert!(
        tool_text.contains("slow-done") && !tool_text.contains("backgrounded"),
        "foreground result must carry the real output directly: {tool_text}"
    );
}
