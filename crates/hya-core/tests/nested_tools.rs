//! Integration tests for `hya-core`: nested tool calls through `ToolCtx::nested`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
};
use hya_core::{AgentCatalog, AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, ModelRef, ToolName, ToolSchema};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{
    Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError, ToolRegistry,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Calls `input.name` with `input.input` through the nested plane and reports
/// what it listed and how the call ended.
struct Nester;

#[async_trait]
impl Tool for Nester {
    fn name(&self) -> &str {
        "nester"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("nester"),
            description: "calls another tool".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let listed: Vec<String> = ctx
            .nested
            .list()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect();
        let name = input["name"].as_str().unwrap_or_default();
        let outcome = match ctx.nested.call(name, input["input"].clone()).await {
            Ok(output) => json!({ "ok": output }),
            Err(error) => {
                let kind = match &error {
                    ToolError::Permission(_) => "permission",
                    ToolError::Input(_) => "input",
                    ToolError::Other(_) => "other",
                    _ => "unexpected",
                };
                json!({ "error": { "kind": kind, "message": error.to_string() } })
            }
        };
        Ok(json!({ "listed": listed, "outcome": outcome }))
    }
}

/// Reports whether its own context can nest.
struct Probe;

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "probe"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("probe"),
            description: "reports its nested plane".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "connected": ctx.nested.is_connected() }))
    }
}

/// One bundle agent whose view is `nester`, `probe`, and `write`.
fn catalog() -> Arc<AgentCatalog> {
    let bundle = Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/nested-test".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: "test-only".to_string(),
        agents: vec![PreparedAgent {
            id: AgentName::new("nesting-agent"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("nesting prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView {
                allow: vec![
                    "harness:tool/nester".to_string(),
                    "harness:tool/probe".to_string(),
                    "harness:tool/write".to_string(),
                ],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
            advertise: Vec::new(),
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    };
    let bundles = BundleCatalog::from_prepared(&[bundle]).expect("valid bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog"))
}

fn nest(name: &str, input: Value) -> FakeStep {
    FakeStep::ToolCall {
        name: "nester".to_string(),
        input: json!({ "name": name, "input": input }),
    }
}

#[tokio::test]
async fn nested_calls_run_in_the_callers_view_and_record_against_the_caller() {
    let workdir = support::TestDir::new("nested-tools");
    let dir = workdir.path().to_path_buf();
    let written = dir.join("a.txt");
    let denied = dir.join("denied.txt");
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            nest("write", json!({ "path": written, "content": "nested\n" })),
            nest("glob", json!({ "pattern": "*" })),
            nest("nester", json!({})),
            nest("write", json!({ "path": denied, "content": "no\n" })),
            nest("probe", json!({})),
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let tools = ToolRegistry::builtins();
    tools.register(Arc::new(Nester)).unwrap();
    tools.register(Arc::new(Probe)).unwrap();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Tool, "nester", Mode::Allow),
        Rule::new(Action::Tool, "probe", Mode::Allow),
        Rule::new(Action::Edit, "**", Mode::Allow),
        Rule::new(Action::Edit, denied.to_string_lossy(), Mode::Deny),
    ]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(provider))),
        Arc::new(RuntimeRegistry::new(tools, catalog())),
        permission,
        EventBus::default(),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("nesting-agent"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "nest".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("nesting-agent"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: dir.clone(),
        reasoning: None,
    };
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    let events: Vec<Event> = engine
        .replay(session)
        .await
        .unwrap()
        .into_iter()
        .map(|envelope| envelope.event)
        .collect();
    // (f) Only the five model calls were requested; nested calls left no
    // request, result, or error of their own.
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::ToolCallRequested { call, name, .. } => Some((*call, name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert!(calls.iter().all(|(_, name)| *name == "nester"), "{calls:?}");
    let results: BTreeMap<_, _> = events
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { call, output, .. } => Some((*call, output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 5, "{events:?}");
    let outcome = |index: usize| results[&calls[index].0]["outcome"].clone();

    // The listed set is the caller's view minus the caller itself.
    let listed = &results[&calls[0].0]["listed"];
    let listed: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert!(
        listed.contains(&"write") && listed.contains(&"probe"),
        "{listed:?}"
    );
    assert!(
        !listed.contains(&"nester") && !listed.contains(&"glob"),
        "{listed:?}"
    );

    // (a) The nested write changed the file and is recorded on the caller.
    assert!(outcome(0).get("ok").is_some(), "{:?}", outcome(0));
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "nested\n");
    let changed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::FilesChanged { call, files, .. } => Some((*call, files.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].0, Some(calls[0].0));
    assert_eq!(changed[0].1[0].path, written.to_string_lossy());

    // (b) A tool outside the view is unknown.
    assert_eq!(outcome(1)["error"]["kind"], "other");
    assert_eq!(outcome(1)["error"]["message"], "unknown tool: glob");
    // (c) A tool cannot call itself.
    assert_eq!(outcome(2)["error"]["kind"], "input");
    // (d) A denial comes back as a permission error and writes nothing.
    assert_eq!(
        outcome(3)["error"]["kind"],
        "permission",
        "{:?}",
        outcome(3)
    );
    assert!(!denied.exists());
    // (e) The nested tool cannot nest further.
    assert_eq!(outcome(4)["ok"]["connected"], false);
}
