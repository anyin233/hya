//! Kernel invariants (ADR-0033): pairing, failures and cancellation as
//! observations, retries, deadlines, rewrite/compaction, workspace snapshots,
//! and reasoning-drop detection.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_context::rewrite::{ModelWindow, Pressure, RewriteCtx, Rewriter, SessionLog, Transcript};
use hya_core::CoreError;
use hya_core::kernel::{self, View, request::AttachmentData};
use hya_engine::{AgentSpec, SessionEngine};
use hya_proto::context::tool_result_text;
use hya_proto::{
    AgentName, CompactionStrategy, Envelope, Event, FileState, FinishReason, Message, MessageId,
    ModelRef, Part, PartId, Projection, Role, SessionId, SessionKind, TokenUsage, ToolName,
    ToolPartState, ToolSchema,
};
use hya_provider::{
    AnthropicMessagesProtocol, COMPACT_CONTEXT_MARKER, Capabilities, CompletionRequest,
    EventStream, FakeProvider, FakeStep, OpenAiChatProtocol, Protocol, Provider, ProviderError,
    ProviderRouter, ReasoningEffort, ReasoningReplayPolicy,
};
use hya_store::SessionStore;
use hya_tool::deadline::{self, Stage};
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const CHECK: &str = "[hya] Completion check, sent once before this run ends.";
const EMPTY_TURN: &str = "[hya] Your last turn ended with thinking only";
const REASONING_NOTICE: &str = "[hya] Reasoning replay notice:";
const BOARD_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";
const BASH_YIELD_COMMAND: &str =
    "echo $$ > pgid; echo first; while [ ! -e go ]; do sleep 0.01; done; echo second; exit 7";

// ============================================================================
// Shared fixtures & helpers
// ============================================================================

struct Fixture {
    engine: Arc<tokio::sync::OnceCell<SessionEngine>>,
    model_fallbacks: std::collections::HashMap<ModelRef, Vec<ModelRef>>,
    _dir: Option<super::support::TestDir>,
    store: SessionStore,
    provider: Arc<FakeProvider>,
    router: ProviderRouter,
    model: ModelRef,
    session: SessionId,
    dir: PathBuf,
    tools: Vec<Arc<dyn Tool>>,
}

async fn fixture(name: &str, provider: FakeProvider) -> Fixture {
    let dir = super::support::TestDir::new(name);
    let store = SessionStore::connect_memory().await.unwrap();
    let mut fixture = fixture_with_store(dir.path().to_path_buf(), provider, store).await;
    fixture._dir = Some(dir);
    fixture
}

async fn fixture_with_store(dir: PathBuf, provider: FakeProvider, store: SessionStore) -> Fixture {
    let provider = Arc::new(provider);
    let router = ProviderRouter::new().with(provider.clone());
    let model = ModelRef::new("fake/model");
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new(hya_engine::BARE_AGENT_ID),
                model: model.clone(),
                workdir: dir.display().to_string(),
                project: None,
                kind: SessionKind::Project,
            }],
        )
        .await
        .unwrap();
    Fixture {
        engine: Default::default(),
        model_fallbacks: Default::default(),
        _dir: None,
        store,
        provider,
        router,
        model,
        session,
        dir,
        tools: kernel::tools::builtin()
            .into_iter()
            .filter(|tool| !hya_tool::is_reserved_kernel_tool(tool.name()))
            .collect(),
    }
}

impl Fixture {
    async fn turn(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.turn_with_publish(prompt, cancel, &|_| {}).await
    }

    async fn begin(&self, prompt: &str) -> MessageId {
        let session = self.session;
        let (user, part, message) = (MessageId::new(), PartId::new(), MessageId::new());
        self.store
            .append_events(
                session,
                vec![
                    Event::MessageStarted {
                        session,
                        message: user,
                        role: Role::User,
                        agent: None,
                        model: None,
                    },
                    Event::TextStart {
                        session,
                        message: user,
                        part,
                    },
                    Event::TextDelta {
                        session,
                        message: user,
                        part,
                        delta: prompt.to_string(),
                    },
                    Event::TextEnd {
                        session,
                        message: user,
                        part,
                    },
                    Event::MessageFinished {
                        session,
                        message: user,
                        role: Role::User,
                        finish: FinishReason::Stop,
                        tokens: None,
                        cause: None,
                    },
                    Event::MessageStarted {
                        session,
                        message,
                        role: Role::Assistant,
                        agent: None,
                        model: None,
                    },
                ],
            )
            .await
            .unwrap();
        message
    }

    async fn turn_with_publish(
        &self,
        prompt: &str,
        cancel: &CancellationToken,
        publish: &(dyn Fn(Envelope) + Send + Sync),
    ) -> Result<FinishReason, CoreError> {
        let engine = self.engine().await;
        engine
            .admit_user_prompt(self.session, prompt.into())
            .await?;
        let mut events = engine.bus().subscribe();
        let agent = AgentSpec {
            name: AgentName::new(hya_engine::BARE_AGENT_ID),
            model: self.model.clone(),
            system_prompt: String::new(),
            workdir: self.dir.clone(),
            reasoning: None,
        };
        let run = engine.run_turn(self.session, &agent, cancel.clone());
        tokio::pin!(run);
        loop {
            tokio::select! {
                biased;
                event = events.recv() => publish(event.unwrap()),
                result = &mut run => {
                    while let Ok(event) = events.try_recv() {
                        publish(event);
                    }
                    return result;
                }
            }
        }
    }

    async fn engine(&self) -> &SessionEngine {
        self.engine
            .get_or_init(|| async {
                let permission = tool_ctx(&self.dir, &CancellationToken::new()).permission;
                super::support::kernel_engine(
                    self.store.clone(),
                    Arc::new(self.router.clone()),
                    permission,
                    &self.tools,
                )
                .with_model_fallbacks(self.model_fallbacks.clone())
            })
            .await
    }

    async fn log(&self) -> Vec<Event> {
        self.store
            .replay(self.session)
            .await
            .unwrap()
            .into_iter()
            .map(|envelope| envelope.event)
            .collect()
    }
}

fn tool_ctx(workdir: &Path, cancel: &CancellationToken) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Edit, "*", Mode::Allow),
        Rule::new(Action::Bash, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]));
    ToolCtx::new(workdir.to_path_buf(), permission, cancel.clone())
}

fn builtin_tool(name: &str) -> Arc<dyn Tool> {
    kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap()
}

fn call(name: &str, input: serde_json::Value) -> Vec<FakeStep> {
    vec![FakeStep::ToolCall {
        name: name.to_string(),
        input,
    }]
}

fn text(body: &str) -> Vec<FakeStep> {
    vec![FakeStep::Text(body.to_string())]
}

fn tool_round(name: &str, input: serde_json::Value) -> Vec<FakeStep> {
    let mut steps = call(name, input);
    steps.push(FakeStep::Finish(FinishReason::ToolCalls));
    steps
}

fn thinking_only(thought: &str, output: Option<u64>) -> Vec<FakeStep> {
    let mut steps = vec![FakeStep::Reasoning(thought.into())];
    if let Some(output) = output {
        steps.push(FakeStep::Usage(TokenUsage {
            output,
            ..TokenUsage::default()
        }));
    }
    steps.push(FakeStep::Finish(FinishReason::Stop));
    steps
}

fn assert_paired(log: &[Event]) {
    let mut requested = HashSet::new();
    let mut open = Vec::new();
    for event in log {
        match event {
            Event::ToolCallRequested { call, .. } => {
                requested.insert(*call);
                open.push(*call);
            }
            Event::ToolResult { call, .. } | Event::ToolError { call, .. } => {
                assert!(
                    requested.contains(call),
                    "result for unrequested call {call}"
                );
                open.retain(|open| open != call);
            }
            Event::StepStarted { .. } => {
                assert!(
                    open.is_empty(),
                    "calls {open:?} have no result before the next step"
                );
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "calls {open:?} have no result");
}

fn tool_event_order(log: &[Event]) -> Vec<&Event> {
    log.iter()
        .filter(|event| {
            matches!(
                event,
                Event::ToolInputStart { .. }
                    | Event::ToolCallRequested { .. }
                    | Event::StepFinished {
                        finish: FinishReason::ToolCalls,
                        ..
                    }
                    | Event::ToolResult { .. }
                    | Event::ToolError { .. }
            )
        })
        .collect()
}

fn tool_errors(log: &[Event]) -> Vec<&str> {
    log.iter()
        .filter_map(|event| match event {
            Event::ToolError { message_text, .. } => Some(message_text.as_str()),
            _ => None,
        })
        .collect()
}

fn assistant_parts(request: &CompletionRequest) -> impl Iterator<Item = &Part> {
    request.messages.iter().flat_map(|message| match message {
        Message::Assistant { parts, .. } => parts.as_slice(),
        _ => &[],
    })
}

fn part_ids(request: &CompletionRequest) -> HashSet<PartId> {
    assistant_parts(request)
        .map(|part| match part {
            Part::Text { id, .. }
            | Part::Reasoning { id, .. }
            | Part::Media { id, .. }
            | Part::Tool { id, .. } => *id,
        })
        .collect()
}

fn is_check(message: &Message) -> bool {
    matches!(message, Message::User { parts, .. }
        if matches!(parts.as_slice(), [Part::Text { text, .. }] if text.starts_with(CHECK)))
}

fn checks_logged(log: &[Event]) -> usize {
    log.iter()
        .filter(|event| matches!(event, Event::TextDelta { delta, .. } if delta.starts_with(CHECK)))
        .count()
}

fn notices_logged(log: &[Event]) -> Vec<&str> {
    log.iter()
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } if delta.starts_with(EMPTY_TURN) => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

async fn last_answer(fx: &Fixture) -> Option<String> {
    let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
    let reply = projection
        .session
        .messages
        .iter()
        .rfind(|message| message.role == Role::Assistant)
        .unwrap();
    reply.parts.iter().rev().find_map(|part| match part {
        hya_proto::PartProjection::Text { text, .. } => Some(text.clone()),
        _ => None,
    })
}

// ============================================================================
// 1. Bare deadline notices & clock (deterministic in-process via ScopedDeadline)
// ============================================================================

fn result_output(request: &CompletionRequest) -> Vec<&str> {
    assistant_parts(request)
        .filter_map(|part| match part {
            Part::Tool {
                state: ToolPartState::Completed { output, .. },
                ..
            } => output["output"].as_str(),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn engine_deadline_notices() {
    for (case, budget, cutoff, scenario) in [
        (
            "final",
            40u64,
            180u64,
            Some((Stage::Final, false, "HEADLESS RUN — FINAL ")),
        ),
        (
            "wrap-up-above",
            380,
            180,
            Some((Stage::WrapUp, true, "HEADLESS RUN — TIME: ")),
        ),
        (
            "wrap-up-below",
            380,
            180,
            Some((Stage::WrapUp, false, "HEADLESS RUN — TIME: ")),
        ),
        ("900", 900, 216, None),
        ("1800", 1800, 432, None),
    ] {
        let cutoff = Duration::from_secs(cutoff);
        let Some((stage, send_check, prefix)) = scenario else {
            let deadline = deadline::install_scoped(Duration::from_secs(budget));
            assert_eq!(2 * deadline.final_window(), cutoff, "{case}");
            continue;
        };
        let fx = fixture(
            &format!("deadline-{case}"),
            FakeProvider::scripted_turns(vec![
                call("bash", json!({"command": "true"})),
                call("bash", json!({"command": "true"})),
                text("done"),
                text("unreached"),
            ]),
        )
        .await;
        let deadline = deadline::install_scoped(Duration::from_secs(budget));
        assert_eq!(2 * deadline.final_window(), cutoff, "{case}");
        if stage == Stage::WrapUp {
            let target_remaining = if send_check {
                deadline.wrap_up_window()
            } else {
                cutoff - Duration::from_secs(5)
            };
            deadline.advance_clock(deadline.remaining().saturating_sub(target_remaining));
        }
        assert_eq!(deadline.stage(), stage, "{case}");
        assert_eq!(
            fx.turn("write the deliverable", &CancellationToken::new())
                .await
                .unwrap(),
            FinishReason::Stop
        );
        let log = fx.log().await;
        assert_eq!(deadline.stage(), stage, "{case}");
        assert_eq!(deadline.remaining() > cutoff, send_check, "{case}");
        assert_eq!(checks_logged(&log), 0, "{case}");
        let requests = fx.provider.requests();
        assert_eq!(requests.len(), 3, "{case}");
        assert!(!format!("{:?}", requests[0].messages).contains(prefix));
        let output = result_output(&requests[1]);
        let [first] = output.as_slice() else {
            panic!("{output:?}")
        };
        let (_, text) = first
            .split_once("\n\n")
            .expect("notice on first tool result");
        assert!(text.starts_with(prefix), "{case}: {text}");
        if stage == Stage::Final {
            let left = text
                .strip_prefix(prefix)
                .and_then(|rest| rest.split_once("s: "))
                .and_then(|(seconds, _)| seconds.parse::<u64>().ok())
                .expect("Final notice with seconds left");
            assert!((30..=40).contains(&left), "{text}");
            assert!(text.contains("of 40s used. Write any missing deliverable"));
            assert!(
                text.ends_with(
                    "then end your turn. If a delivered file already passes your checks, do not \
                     edit it in place — test any change on a copy first and replace the file only \
                     if the copy still passes. If a delivered file is unverified, verify it now \
                     and fix it directly only if it fails. Start only commands that finish well \
                     within the time left."
                ),
                "{text}"
            );
        } else {
            assert!(text.contains(&format!("of {budget}s used")), "{text}");
            assert!(
                text.contains("the request's own checks, then finish"),
                "{text}"
            );
        }
        assert!(requests.iter().all(|req| req.system == requests[0].system));
        let logged: Vec<_> = log
            .iter()
            .filter_map(|event| {
                let Event::ToolResult { output, .. } = event else {
                    return None;
                };
                output["output"]
                    .as_str()?
                    .split_once(prefix)
                    .map(|(_, rest)| rest)
            })
            .collect();
        assert_eq!(logged, vec![text.strip_prefix(prefix).unwrap()]);
    }
}

#[tokio::test]
async fn engine_deadline_clock() {
    let fx = fixture(
        "deadline-clock",
        FakeProvider::scripted_turns(vec![
            text("ready"),
            call("bash", json!({"command": "true"})),
            call("todo", json!({"ops": []})),
            text("first update"),
            call("bash", json!({"command": "true"})),
            text("second update"),
        ]),
    )
    .await;
    let tenth = Duration::from_secs(90);
    let deadline = deadline::install_scoped(tenth * 10);
    let cancel = CancellationToken::new();
    assert_eq!(fx.turn("start", &cancel).await.unwrap(), FinishReason::Stop);
    for _ in 0..2 {
        deadline.advance_clock(tenth);
        assert_eq!(
            fx.turn("continue", &cancel).await.unwrap(),
            FinishReason::Stop
        );
    }
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 6);
    let clocks = |request: &CompletionRequest| {
        result_output(request)
            .into_iter()
            .flat_map(str::lines)
            .filter(|line| line.starts_with("HEADLESS RUN — clock: "))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert!(clocks(&requests[0]).is_empty());
    let first = clocks(&requests[2]);
    assert_eq!(first.len(), 1, "first tenth must reach the model");
    assert_eq!(
        clocks(&requests[3]),
        first,
        "same tenth must not add a clock line"
    );
    let next = clocks(&requests[5]);
    assert_eq!(next.len(), 2, "next tenth must reach the model");
    assert_ne!(next[1], first[0]);
    assert!(assistant_parts(&requests[3]).any(|part| matches!(
        part,
        Part::Tool { name, state: ToolPartState::Completed { output, .. }, .. }
            if name.as_str() == "todo" && output == &json!(hya_proto::context::render_todos(&[]))
    )));
    assert!(requests.iter().all(|req| req.system == requests[0].system));
    let log = fx.log().await;
    let logged: Vec<_> = log
        .iter()
        .filter_map(|event| {
            let Event::ToolResult { output, .. } = event else {
                return None;
            };
            output["output"]
                .as_str()?
                .lines()
                .find(|line| line.starts_with("HEADLESS RUN — clock: "))
        })
        .collect();
    assert_eq!(logged, next);
}

#[tokio::test]
async fn empty_turn_in_final_stage_stops_without_continuation() {
    let fx = fixture(
        "empty-turn-final",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            thinking_only("out of time", None),
            text("done"),
        ]),
    )
    .await;
    let deadline = deadline::install_scoped(Duration::from_secs(40));
    assert_eq!(deadline.stage(), Stage::Final);
    assert_eq!(
        fx.turn("do it", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(fx.provider.requests().len(), 2);
    let log = fx.log().await;
    assert!(notices_logged(&log).is_empty());
    assert_eq!(checks_logged(&log), 0, "no check near deadline");
    assert_eq!(last_answer(&fx).await.as_deref(), None);
}

// ============================================================================
// 2. Pairing, failures, invalid argument JSON, completion checks & empty turns
// ============================================================================

#[tokio::test]
async fn a1_tool_results_pair_with_calls_and_carry_reasoning_default() {
    let fx = fixture(
        "a1",
        FakeProvider::scripted_turns(vec![
            tool_round("write", json!({"path": "out/a.txt", "content": "hello"})),
            tool_round("bash", json!({"command": "cat out/a.txt"})),
            text("done"),
        ])
        .with_reasoning_default(ReasoningEffort::Max),
    )
    .await;
    let finish = fx.turn("write then cat", &CancellationToken::new()).await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert!(
        fx.provider
            .requests()
            .iter()
            .all(|req| req.reasoning == Some(ReasoningEffort::Max))
    );
    let log = fx.log().await;
    assert_paired(&log);
    assert!(matches!(
        tool_event_order(&log).as_slice(),
        [Event::ToolInputStart { call: first, .. },
         Event::ToolCallRequested { call: first_request, .. },
         Event::StepFinished { step: 0, .. },
         Event::ToolResult { call: first_result, .. },
         Event::ToolInputStart { call: second, .. },
         Event::ToolCallRequested { call: second_request, .. },
         Event::StepFinished { step: 1, .. },
         Event::ToolResult { call: second_result, .. }]
            if first == first_request && first == first_result
                && second == second_request && second == second_result
    ));
    let outputs: Vec<_> = log
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["bytes"], 5);
    assert_eq!(outputs[0]["ok"], true);
    assert_eq!(outputs[1]["exit_code"], 0);
    assert_eq!(outputs[1]["output"], "hello\n[exit code: 0]");
}

#[tokio::test]
async fn a2_failures_are_observations() {
    let fx = fixture(
        "a2",
        FakeProvider::scripted_turns(vec![
            call("no_such_tool", json!({})),
            call("edit", json!({"path": "a.txt", "new": "x"})),
            call("read", json!({"path": "missing.txt"})),
            text("next"),
            text("checked"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("fail three ways", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let log = fx.log().await;
    assert_paired(&log);
    let errors = tool_errors(&log);
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert_eq!(errors[0], "unknown tool: no_such_tool");
    assert!(errors[1].contains("old"), "{}", errors[1]);
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    let failed = assistant_parts(&requests[3])
        .filter(|part| {
            matches!(
                part,
                Part::Tool {
                    state: ToolPartState::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(failed, 3);
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::TextReplace { text, .. } if text == "next"))
    );
}

fn decoded_input(args: &str) -> serde_json::Value {
    let args = serde_json::to_string(args).unwrap();
    let mut decoder = OpenAiChatProtocol.decoder(SessionId::new(), MessageId::new(), None);
    let mut events = decoder
        .push(&format!(
            r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"c","function":{{"name":"x","arguments":{args}}}}}]}},"finish_reason":"length"}}]}}"#
        ))
        .unwrap();
    events.extend(decoder.finish().unwrap());
    events
        .into_iter()
        .find_map(|event| match event {
            Event::ToolCallRequested { input, .. } => Some(input),
            _ => None,
        })
        .unwrap()
}

#[derive(Default)]
struct Optional(AtomicUsize);

#[async_trait::async_trait]
impl Tool for Optional {
    fn name(&self) -> &str {
        "optional"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new("optional"),
            description: "Takes only optional arguments.".to_string(),
            input_schema: json!({"type": "object", "properties": {"note": {"type": "string"}}}),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!("ran"))
    }
}

#[tokio::test]
async fn invalid_argument_json_fails_without_running() {
    let mut fx = fixture(
        "invalid-args",
        FakeProvider::scripted_turns(vec![
            call("bash", decoded_input(r#"{"command":"ec"#)),
            call("optional", decoded_input(r#"{"note":"a"}}"#)),
            text("done"),
        ]),
    )
    .await;
    let optional = Arc::new(Optional::default());
    fx.tools.push(optional.clone());
    assert_eq!(
        fx.turn("call with broken arguments", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(optional.0.load(Ordering::SeqCst), 0);
    let log = fx.log().await;
    assert_paired(&log);
    let requests = fx.provider.requests();
    for (request, error) in [
        (
            &requests[1],
            "EOF while parsing a string at line 1 column 14",
        ),
        (&requests[2], "trailing characters at line 1 column 13"),
    ] {
        let message = assistant_parts(request)
            .filter_map(|part| match part {
                Part::Tool {
                    state: ToolPartState::Error { message, .. },
                    ..
                } => Some(message.as_str()),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(
            message,
            format!(
                "arguments are not valid JSON: {error}. Nothing was run; call the tool \
                 again with its arguments as one valid JSON object."
            )
        );
        AnthropicMessagesProtocol.encode(request).unwrap();
    }
}

#[tokio::test]
async fn completion_check_belongs_to_headless_driver() {
    let fx = fixture(
        "completion-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("draft"),
            call("todo", json!({"ops": []})),
            text("final"),
            text("unreached"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("do it", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    // Headless continuation is owned by the driver; the turn ends on its answer.
    assert_eq!(checks_logged(&fx.log().await), 0);
    assert_eq!(last_answer(&fx).await.as_deref(), Some("draft"));
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].messages.iter().any(is_check));
    let text = kernel::COMPLETION_CHECK;
    assert!(text.starts_with(CHECK));
    assert_eq!(
        text.lines().nth(2),
        Some(concat!(
            "2. For each item, look at the result as it is now (the file on disk, the output of the real command) ",
            "and compare it with the request's wording. Where the wording is exact, print the actual text and read ",
            "it against the request's text, character for character. A program or pattern you wrote to do the ",
            "comparison does not count, and neither does a substring or a summary. Where the request says a value ",
            "is unknown, try the result with a value other than the one you can see."
        ))
    );
    fx.turn("explicit follow-up", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(!requests[3].messages.iter().any(is_check));
    assert_eq!(last_answer(&fx).await.as_deref(), Some("final"));
    let parts: Vec<_> = assistant_parts(&requests[3]).collect();
    assert!(matches!(
        parts.as_slice(),
        [
            Part::Tool { .. },
            Part::Text { text, .. },
            Part::Tool { .. },
        ] if text == "draft"
    ));
}

#[tokio::test]
async fn completion_check_skips_ineligible_runs() {
    for (case, script, expected_finish, expected_reqs) in [
        (
            "no-tools",
            vec![text("hi"), text("unreached")],
            FinishReason::Stop,
            1,
        ),
        (
            "unknown-tool",
            vec![call("nope", json!({})), text("done"), text("unreached")],
            FinishReason::Stop,
            2,
        ),
        (
            "error-finish",
            vec![
                call("todo", json!({"ops": []})),
                vec![
                    FakeStep::Text("filtered".into()),
                    FakeStep::Finish(FinishReason::Error),
                ],
                text("unreached"),
            ],
            FinishReason::Error,
            2,
        ),
    ] {
        let fx = fixture(
            &format!("no-check-{case}"),
            FakeProvider::scripted_turns(script),
        )
        .await;
        assert_eq!(
            fx.turn("go", &CancellationToken::new()).await.unwrap(),
            expected_finish,
            "{case}"
        );
        assert_eq!(fx.provider.requests().len(), expected_reqs, "{case}");
        assert_eq!(checks_logged(&fx.log().await), 0, "{case}");
    }

    // Cancel as the answer ends logs no check.
    let fx = fixture(
        "cancel-no-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("done"),
            text("unreached"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    let trip = cancel.clone();
    let finish = fx
        .turn_with_publish("go", &cancel, &move |envelope| {
            if matches!(envelope.event, Event::StepFinished { step: 1, .. }) {
                trip.cancel();
            }
        })
        .await;
    assert_eq!(finish.unwrap(), FinishReason::Stop);
    assert_eq!(fx.provider.requests().len(), 2);
    assert_eq!(checks_logged(&fx.log().await), 0);
}

#[tokio::test]
async fn logged_completion_check_survives_wording_change() {
    let fx = fixture(
        "check-replay",
        FakeProvider::scripted_turns(vec![
            text("draft"),
            call("todo", json!({"ops": []})),
            text("final"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    fx.turn("do it", &cancel).await.unwrap();

    let old_check = "[hya] Completion check, sent once before this run ends. Compare the result with the request itself, not with your memory of it.\n\
1. Re-read the request. List what it states literally: paths and file names, exact strings and output formats, commands and flags, values and counts, and anything it says is unknown or must not be assumed.\n\
2. For each item, look at the result as it is now (the file on disk, the output of the real command) and compare it with the request's wording, character for character where the wording is exact. A check that only shows what you expected to see (a pattern you wrote, a substring, a summary) does not count.\n\
3. If something differs, fix it and check again. If everything already matches, change nothing.\n\
Then give your final answer.";
    fx.store
        .append_events(
            fx.session,
            kernel::round::system_message_events(fx.session, MessageId::new(), old_check.into()),
        )
        .await
        .unwrap();
    let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
    let messages = kernel::request::projection_to_messages(
        &AgentName::new(hya_engine::BARE_AGENT_ID),
        &projection,
        &fx.model,
        &AttachmentData::new(),
    );
    let is_logged_notice =
        |msg: &Message| matches!(msg, Message::System { content, .. } if content == old_check);
    let rendered_as_system = messages.last().is_some_and(is_logged_notice);

    fx.turn("continue", &cancel).await.unwrap();
    let requests = fx.provider.requests();
    assert_eq!(
        (
            rendered_as_system,
            requests.len(),
            checks_logged(&fx.log().await)
        ),
        (true, 3, 1)
    );
    assert!(
        requests[1..]
            .iter()
            .all(|req| req.messages.iter().any(is_logged_notice)
                && req.messages.iter().all(|msg| !is_check(msg)))
    );
}

#[tokio::test]
async fn empty_turn_thinking_only_and_whitespace_recovery() {
    // Thinking-only with output tokens: notice logged with token count & thought tail,
    // transient across rounds, reasoning provider_data replayed.
    let fx = replaying_fixture(
        "empty-turn",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            thinking_only("a long thought", Some(65_536)),
            text("draft"),
            text("final"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("do it", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let log = fx.log().await;
    let notices = notices_logged(&log);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("It ended after 65536 output tokens."));
    assert_eq!(checks_logged(&log), 0);
    assert_eq!(last_answer(&fx).await.as_deref(), Some("draft"));
    let empty_end = log
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::StepFinished {
                    step: 1,
                    finish: FinishReason::Stop,
                    ..
                }
            )
        })
        .unwrap();
    assert!(matches!(
        &log[empty_end + 1..empty_end + 7],
        [
            Event::MessageStarted { role: Role::System, .. },
            Event::TextStart { .. },
            Event::TextDelta { delta, .. },
            Event::TextEnd { .. },
            Event::MessageFinished { role: Role::System, .. },
            Event::StepStarted { step: 2, .. },
        ] if delta.starts_with(EMPTY_TURN)
    ));
    fx.turn("next task", &CancellationToken::new())
        .await
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(matches!(
        requests[2].messages.last(),
        Some(Message::User { parts, .. })
        if matches!(parts.as_slice(), [Part::Text { text, .. }] if text.ends_with("Where your previous thinking left off:\na long thought"))
    ));
    assert!(!requests[3].messages.iter().any(|message| matches!(
        message,
        Message::User { parts, .. }
            if matches!(parts.as_slice(), [Part::Text { text, .. }] if text.starts_with(EMPTY_TURN))
    )));
    for request in &requests[2..] {
        assert!(assistant_parts(request).any(|part| matches!(
            part,
            Part::Reasoning { text, provider_data: Some(data), .. } if text == "a long thought" && data == &summary_item()
        )));
    }

    // Whitespace with a length-limited finish is recovered as an empty turn.
    let ws = fixture(
        "empty-turn-whitespace",
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Text(" \n\t ".into()),
                FakeStep::Finish(FinishReason::Length),
            ],
            text("done"),
        ]),
    )
    .await;
    assert_eq!(
        ws.turn("do it", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(ws.provider.requests().len(), 2);
    assert_eq!(notices_logged(&ws.log().await).len(), 1);
    assert_eq!(last_answer(&ws).await.as_deref(), Some("done"));
}

#[tokio::test]
async fn empty_turn_limit_reset_and_post_check_behavior() {
    // Third consecutive empty turn ends the run with 2 notices (no token sentence without usage).
    let limit = fixture(
        "empty-turn-limit",
        FakeProvider::scripted_turns(vec![
            thinking_only("one", None),
            thinking_only("two", None),
            thinking_only("three", None),
            text("unreached"),
        ]),
    )
    .await;
    assert_eq!(
        limit
            .turn("do it", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(limit.provider.requests().len(), 3);
    let log = limit.log().await;
    let notices = notices_logged(&log);
    assert_eq!(notices.len(), 2);
    assert!(notices.iter().all(|n| !n.contains("output tokens")));
    assert_eq!(last_answer(&limit).await, None);

    // A tool call resets the consecutive empty-turn count.
    let reset = fixture(
        "empty-turn-reset",
        FakeProvider::scripted_turns(vec![
            thinking_only("one", None),
            thinking_only("two", None),
            call("todo", json!({"ops": []})),
            thinking_only("three", None),
            thinking_only("four", None),
            text("draft"),
            text("final"),
        ]),
    )
    .await;
    assert_eq!(
        reset
            .turn("do it", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(reset.provider.requests().len(), 6);
    let reset_log = reset.log().await;
    assert_eq!(notices_logged(&reset_log).len(), 4);
    assert_eq!(checks_logged(&reset_log), 0);
    assert_eq!(last_answer(&reset).await.as_deref(), Some("draft"));

    // An empty turn after the completion check gets no second check.
    let after = fixture(
        "empty-turn-after-check",
        FakeProvider::scripted_turns(vec![
            call("todo", json!({"ops": []})),
            text("draft"),
            thinking_only("checking", Some(12)),
            text("final"),
            text("unreached"),
        ]),
    )
    .await;
    assert_eq!(
        after
            .turn("do it", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    after
        .engine()
        .await
        .inject_system_message(after.session, kernel::COMPLETION_CHECK.into())
        .await
        .unwrap();
    assert_eq!(
        after
            .turn("follow-up", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(after.provider.requests().len(), 4);
    let after_log = after.log().await;
    assert_eq!(checks_logged(&after_log), 1);
    assert_eq!(notices_logged(&after_log).len(), 1);
    assert_eq!(last_answer(&after).await.as_deref(), Some("final"));
}

// ============================================================================
// 3. Retries & stream breaks
// ============================================================================

enum Attempt {
    Play,
    Open(ProviderError),
    Stream(Vec<FakeStep>, ProviderError),
    UnfinishedTool(Vec<FakeStep>, ProviderError),
    Break(Vec<FakeStep>, ProviderError),
}

struct Flaky {
    fake: FakeProvider,
    failures: Mutex<VecDeque<Attempt>>,
    requests: Mutex<Vec<CompletionRequest>>,
    clock: RetryClock,
}

#[async_trait::async_trait]
impl Provider for Flaky {
    fn id(&self) -> &str {
        self.fake.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.fake.capabilities(model)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.clock.finish_wait();
        self.requests.lock().unwrap().push(req.clone());
        let attempt = self.failures.lock().unwrap().pop_front();
        let (partial, error, finished, tool_input_only) = match attempt {
            None | Some(Attempt::Play) => return self.fake.stream(req, session, message).await,
            Some(Attempt::Open(error)) => {
                self.clock.pause();
                return Err(error);
            }
            Some(Attempt::Stream(partial, error)) => (partial, error, false, false),
            Some(Attempt::UnfinishedTool(partial, error)) => (partial, error, false, true),
            Some(Attempt::Break(script, error)) => (script, error, true, false),
        };
        let mut events = FakeProvider::materialize(&partial, session, message);
        if tool_input_only {
            events.truncate(2);
        } else if !finished {
            events.pop();
        }
        let clock = self.clock.clone();
        let items = events
            .into_iter()
            .map(Ok)
            .chain([Err(error)])
            .inspect(move |item| {
                if item.is_err() {
                    clock.pause();
                }
            });
        Ok(Box::pin(futures::stream::iter(items)))
    }
}

fn server_error(message: &str) -> ProviderError {
    ProviderError::HttpStatus {
        status: 500,
        message: format!("in-stream error (server_error): {message}"),
        retry_after: None,
    }
}

fn body_break() -> ProviderError {
    ProviderError::StreamInterrupted("Transport error: error decoding response body".into())
}

async fn flaky_fixture(
    name: &str,
    fake: Vec<Vec<FakeStep>>,
    failures: Vec<Attempt>,
) -> (Fixture, Arc<Flaky>) {
    static RETRY_SUBSCRIBER: std::sync::Once = std::sync::Once::new();
    RETRY_SUBSCRIBER.call_once(|| tracing::subscriber::set_global_default(RetryEvents).unwrap());
    let mut fx = fixture(name, FakeProvider::scripted_turns(Vec::new())).await;
    let flaky = Arc::new(Flaky {
        fake: FakeProvider::scripted_turns(fake),
        failures: Mutex::new(failures.into()),
        requests: Mutex::default(),
        clock: RetryClock::default(),
    });
    fx.router = ProviderRouter::new().with(flaky.clone());
    (fx, flaky)
}

#[derive(Clone, Default)]
struct RetryClock(Arc<Mutex<RetryClockState>>);

#[derive(Default)]
struct RetryClockState {
    pending: Option<PendingWait>,
    waits: Vec<Duration>,
}

struct PendingWait {
    started: tokio::time::Instant,
    hold_clock: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RetryClock {
    fn pause(&self) {
        tokio::time::pause();
        let (hold_clock, done) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = done.blocking_recv();
        });
        self.0.lock().unwrap().pending = Some(PendingWait {
            started: tokio::time::Instant::now(),
            hold_clock: Some(hold_clock),
        });
    }

    fn finish_wait(&self) {
        let mut state = self.0.lock().unwrap();
        if let Some(wait) = state.pending.take() {
            if wait.hold_clock.is_none() {
                state.waits.push(wait.started.elapsed());
            }
            tokio::time::resume();
        }
    }

    fn assert_waits(&self, seconds: &[u64]) {
        let state = self.0.lock().unwrap();
        assert_eq!(state.waits.len(), seconds.len(), "{:?}", state.waits);
        for (&actual, &seconds) in state.waits.iter().zip(seconds) {
            let expected = Duration::from_secs(seconds);
            assert!(
                (expected..=expected + Duration::from_millis(1)).contains(&actual),
                "expected {expected:?}, waited {actual:?}"
            );
        }
    }
}

tokio::task_local! {
    static ACTIVE_RETRY_CLOCK: RetryClock;
}

struct RetryEvents;

impl tracing::Subscriber for RetryEvents {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.name() == "round_retry"
    }

    fn event(&self, _: &tracing::Event<'_>) {
        let _ = ACTIVE_RETRY_CLOCK.try_with(|clock| {
            let mut state = clock.0.lock().unwrap();
            let wait = state.pending.as_mut().unwrap();
            drop(wait.hold_clock.take());
        });
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        unreachable!("only round_retry is enabled")
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl Flaky {
    async fn turn(&self, fx: &Fixture, prompt: &str) -> Result<FinishReason, CoreError> {
        let cancel = CancellationToken::new();
        let finish = ACTIVE_RETRY_CLOCK
            .scope(self.clock.clone(), fx.turn(prompt, &cancel))
            .await;
        self.clock.finish_wait();
        finish
    }
}

#[tokio::test]
async fn transient_open_stream_and_interrupted_failures_retry_same_request() {
    // 1. In-stream 500 retries the same request and view, recording error summary.
    let (fx, flaky) = flaky_fixture(
        "retry-transient",
        vec![
            vec![
                FakeStep::Reasoning("plan the todo".into()),
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input: json!({"ops": []}),
                },
            ],
            text("done"),
            text("checked"),
        ],
        vec![
            Attempt::Play,
            Attempt::Stream(Vec::new(), server_error("upstream unreachable")),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "plan").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5]);
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(format!("{:?}", requests[1]), format!("{:?}", requests[2]));
    assert!(
        assistant_parts(&requests[2])
            .any(|p| matches!(p, Part::Reasoning { text, .. } if text == "plan the todo"))
    );
    let log = fx.log().await;
    let steps: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::StepFinished {
                step,
                finish,
                error,
                ..
            } => Some((*step, *finish, error.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(steps[1].0, 1);
    assert_eq!(steps[1].1, FinishReason::Error);
    assert!(
        steps[1]
            .2
            .as_deref()
            .is_some_and(|d| d.contains("upstream unreachable"))
    );
    assert_eq!(steps[2], (2, FinishReason::Stop, None));
    let views: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::StepStarted { step, view, .. } if [1, 2].contains(step) => view.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0], views[1]);

    // 2. Open transport + 429 failures retry with backoff [5, 10] without creating extra steps.
    let (fx, flaky) = flaky_fixture(
        "retry-open",
        vec![
            tool_round("todo", json!({"ops": []})),
            text("done"),
            text("checked"),
        ],
        vec![
            Attempt::Play,
            Attempt::Open(ProviderError::Transport("connection lost".into())),
            Attempt::Open(ProviderError::HttpStatus {
                status: 429,
                message: "rate limited".into(),
                retry_after: None,
            }),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5, 10]);
    let reqs = flaky.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 4);
    for req in &reqs[2..4] {
        assert_eq!(format!("{req:?}"), format!("{:?}", reqs[1]));
    }
    let step_events: Vec<_> = fx
        .log()
        .await
        .into_iter()
        .filter_map(|e| match e {
            Event::StepStarted { step, .. } => Some((step, None)),
            Event::StepFinished { step, finish, .. } => Some((step, Some(finish))),
            _ => None,
        })
        .collect();
    assert_eq!(
        step_events,
        [
            (0, None),
            (0, Some(FinishReason::ToolCalls)),
            (1, None),
            (1, Some(FinishReason::Stop)),
        ]
    );

    // 3. Stream interruption before any part is retried.
    let (fx, flaky) = flaky_fixture(
        "retry-interrupted",
        vec![text("done")],
        vec![
            Attempt::Stream(
                Vec::new(),
                ProviderError::StreamInterrupted("stalled stream: no SSE frame within 300s".into()),
            ),
            Attempt::Stream(Vec::new(), server_error("upstream unreachable")),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5, 10]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn non_transient_and_deadline_blocked_failures_do_not_retry() {
    for (case, attempt, expected) in [
        (
            "open-400",
            Attempt::Open(ProviderError::HttpStatus {
                status: 400,
                message: "bad request".into(),
                retry_after: None,
            }),
            "http status 400: bad request",
        ),
        (
            "stream-400",
            Attempt::Stream(
                Vec::new(),
                ProviderError::HttpStatus {
                    status: 400,
                    message: "in-stream error (invalid_request_error): bad".into(),
                    retry_after: None,
                },
            ),
            "http status 400: in-stream error (invalid_request_error): bad",
        ),
    ] {
        let (fx, flaky) = flaky_fixture(case, vec![text("unreached")], vec![attempt]).await;
        let error = flaky.turn(&fx, "hi").await.expect_err("400 ends the run");
        assert_eq!(error.to_string(), expected, "{case}");
        flaky.clock.assert_waits(&[]);
        assert_eq!(flaky.requests.lock().unwrap().len(), 1, "{case}");
    }

    // Retry wait that would cross Final is skipped.
    let (fx, flaky) = flaky_fixture(
        "retry-deadline",
        vec![text("unreached")],
        vec![Attempt::Stream(
            Vec::new(),
            server_error("upstream unreachable"),
        )],
    )
    .await;
    let deadline = deadline::install_scoped(Duration::from_secs(94));
    assert_eq!(deadline.stage(), Stage::Normal);
    assert!(deadline.remaining() < deadline.final_window() + Duration::from_secs(5));
    let error = flaky
        .turn(&fx, "hi")
        .await
        .expect_err("retry wait would reach Final");
    assert_eq!(
        error.to_string(),
        "http status 500: in-stream error (server_error): upstream unreachable"
    );
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert_eq!(deadline.stage(), Stage::Normal);
}

#[tokio::test]
async fn retry_backoff_nine_attempt_bound_and_round_reset() {
    let backoff = [5, 10, 20, 40, 60, 60, 60, 60];

    // Succeeds on 9th attempt (8 failures).
    let (fx, flaky) = flaky_fixture(
        "retry-ninth-ok",
        vec![text("done")],
        (1..=8)
            .map(|i| Attempt::Stream(Vec::new(), server_error(&format!("attempt {i}"))))
            .collect(),
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&backoff);
    assert_eq!(flaky.requests.lock().unwrap().len(), 9);

    // 9th open failure and 9th stream failure both terminate cleanly.
    for (case, open) in [("open", true), ("stream", false)] {
        let failures = (1..=9)
            .map(|i| {
                if open {
                    Attempt::Open(ProviderError::HttpStatus {
                        status: 500,
                        message: format!("attempt {i}"),
                        retry_after: None,
                    })
                } else {
                    Attempt::Stream(Vec::new(), server_error(&format!("attempt {i}")))
                }
            })
            .collect();
        let (fx, flaky) = flaky_fixture(case, vec![text("unreached")], failures).await;
        let err = flaky.turn(&fx, "hi").await.expect_err("9th fails");
        assert!(err.to_string().ends_with("attempt 9"), "{case}: {err}");
        flaky.clock.assert_waits(&backoff);
        let reqs = flaky.requests.lock().unwrap().clone();
        assert_eq!(reqs.len(), 9, "{case}");
        assert!(
            reqs.iter()
                .all(|r| format!("{r:?}") == format!("{:?}", reqs[0]))
        );
    }

    // A completed round resets the retry counter.
    let failures = (1..=18)
        .map(|i| {
            if i == 9 {
                Attempt::Play
            } else {
                Attempt::Stream(Vec::new(), server_error(&format!("attempt {i}")))
            }
        })
        .collect();
    let (fx, flaky) = flaky_fixture(
        "retry-reset",
        vec![call("todo", json!({"ops": []}))],
        failures,
    )
    .await;
    let err = flaky.turn(&fx, "hi").await.expect_err("18th fails");
    assert!(err.to_string().ends_with("attempt 18"));
    flaky
        .clock
        .assert_waits(&[5, 10, 20, 40, 60, 60, 60, 60, 5, 10, 20, 40, 60, 60, 60, 60]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 18);
}

#[tokio::test]
async fn stream_cut_after_partial_output_retries_cleanly_and_keeps_last_on_exhaustion() {
    let (fx, flaky) = flaky_fixture(
        "retry-partial",
        vec![
            vec![
                FakeStep::Reasoning("plan the todo".into()),
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input: json!({"ops": []}),
                },
            ],
            text("done"),
            text("checked"),
        ],
        vec![
            Attempt::Play,
            Attempt::Stream(
                vec![
                    FakeStep::Reasoning("half a thought".into()),
                    FakeStep::Text("partial answer".into()),
                ],
                server_error("retryable with partial text"),
            ),
        ],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "plan").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[5]);
    let requests = flaky.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(format!("{:?}", requests[1]), format!("{:?}", requests[2]));
    for req in &requests[2..] {
        let sent = format!("{req:?}");
        assert!(!sent.contains("half a thought") && !sent.contains("partial answer"));
    }
    let envelopes = fx.store.replay(fx.session).await.unwrap();
    let log: Vec<_> = envelopes.iter().map(|e| e.event.clone()).collect();
    assert!(
        log.iter()
            .any(|e| matches!(e, Event::ReasoningReplace { text, .. } if text == "half a thought"))
    );
    assert!(
        log.iter()
            .any(|e| matches!(e, Event::TextReplace { text, .. } if text == "partial answer"))
    );
    // Each recorded cut reproduces its live request SHA-256.
    let mut attempts = 0;
    for envelope in &envelopes {
        let Event::StepStarted { view, .. } = &envelope.event else {
            continue;
        };
        let recorded = view.as_ref().expect("step has view");
        let end = envelopes.partition_point(|entry| entry.seq <= recorded.cut);
        let projection = Projection::from_events(&envelopes[..end]);
        let rebuilt = kernel::request::request(
            &View {
                projection: &projection,
            },
            requests[attempts].system.as_deref().unwrap(),
            requests[attempts].tools.clone(),
            &fx.model,
            &AgentName::new(hya_engine::BARE_AGENT_ID),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            kernel::request::request_sha256(&rebuilt).unwrap(),
            recorded.request_sha256
        );
        assert_eq!(format!("{rebuilt:?}"), format!("{:?}", requests[attempts]));
        attempts += 1;
    }
    assert_eq!(attempts, 3);

    let (visible, interrupted) = flaky_fixture(
        "visible-output-interruption",
        vec![text("unreached")],
        vec![Attempt::Stream(
            vec![FakeStep::Text("partial answer".into())],
            body_break(),
        )],
    )
    .await;
    assert!(matches!(
        interrupted.turn(&visible, "plan").await,
        Err(CoreError::Provider(error)) if matches!(*error, ProviderError::StreamInterrupted(_))
    ));
    interrupted.clock.assert_waits(&[]);
    assert_eq!(interrupted.requests.lock().unwrap().len(), 1);
    assert_eq!(
        last_answer(&visible).await.as_deref(),
        Some("partial answer")
    );
    assert!(
        !visible
            .log()
            .await
            .iter()
            .any(|e| matches!(e, Event::PartDeleted { .. }))
    );

    // Exhausting retries after partial output logs all 9 but projects only the 9th.
    let failures = (1..=9)
        .map(|i| {
            Attempt::Stream(
                vec![FakeStep::Reasoning(format!("thought {i}"))],
                server_error(&format!("attempt {i}")),
            )
        })
        .collect();
    let (fx2, flaky2) =
        flaky_fixture("retry-partial-limit", vec![text("unreached")], failures).await;
    let err = flaky2.turn(&fx2, "hi").await.expect_err("9th fails");
    assert!(err.to_string().ends_with("attempt 9"));
    let log2 = fx2.log().await;
    assert_eq!(
        log2.iter()
            .filter(|e| matches!(e, Event::ReasoningReplace { .. }))
            .count(),
        9
    );
    let proj = fx2.store.read_projection(fx2.session).await.unwrap();
    assert!(matches!(
        proj.session.messages.last().unwrap().parts.as_slice(),
        [hya_proto::PartProjection::Reasoning { text, .. }] if text == "thought 9"
    ));
}

#[tokio::test]
async fn stream_cut_during_or_after_tool_call_never_executes_uncommitted_tool() {
    // 1. A confirmed call bars replay; the turn closes it without execution.
    let command = json!({"command": "echo ran >> count.txt"});
    let (fx, flaky) = flaky_fixture(
        "retry-after-tool-call",
        vec![call("bash", command.clone()), text("done"), text("checked")],
        vec![Attempt::Stream(tool_round("bash", command), body_break())],
    )
    .await;
    assert!(matches!(
        flaky.turn(&fx, "count").await,
        Err(CoreError::Provider(error)) if matches!(*error, ProviderError::StreamInterrupted(_))
    ));
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert!(!fx.dir.join("count.txt").exists());
    let log = fx.log().await;
    let calls: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::ToolCallRequested { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    let results: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::ToolResult { call, .. } | Event::ToolError { call, .. } => Some(*call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(results, [calls[0]]);

    // 2. Cut during tool input: PartDeleted emitted and tool never runs.
    let (fx2, flaky2) = flaky_fixture(
        "retry-tool-input",
        vec![text("done")],
        vec![Attempt::UnfinishedTool(
            call("bash", json!({"command": "echo ran >> count.txt"})),
            body_break(),
        )],
    )
    .await;
    assert_eq!(flaky2.turn(&fx2, "hi").await.unwrap(), FinishReason::Stop);
    flaky2.clock.assert_waits(&[5]);
    assert!(!fx2.dir.join("count.txt").exists());
    let log2 = fx2.log().await;
    let started = log2
        .iter()
        .find_map(|e| match e {
            Event::ToolInputStart { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    assert!(
        log2.iter()
            .any(|e| matches!(e, Event::PartDeleted { part, .. } if *part == started))
    );
}

#[tokio::test]
async fn stream_break_after_final_frame_completes_text_and_tool_rounds() {
    // Text round completes without retry or deletion.
    let (fx, flaky) = flaky_fixture(
        "break-after-text",
        vec![text("unreached")],
        vec![Attempt::Break(
            vec![
                FakeStep::Text("the answer".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            body_break(),
        )],
    )
    .await;
    assert_eq!(flaky.turn(&fx, "hi").await.unwrap(), FinishReason::Stop);
    flaky.clock.assert_waits(&[]);
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert!(
        !fx.log()
            .await
            .iter()
            .any(|e| matches!(e, Event::PartDeleted { .. }))
    );
    assert_eq!(last_answer(&fx).await.as_deref(), Some("the answer"));

    // Tool round completes and runs its tool.
    let (fx2, flaky2) = flaky_fixture(
        "break-after-tool",
        vec![text("done"), text("checked")],
        vec![Attempt::Break(
            tool_round("bash", json!({"command": "echo ran >> count.txt"})),
            body_break(),
        )],
    )
    .await;
    assert_eq!(
        flaky2.turn(&fx2, "count").await.unwrap(),
        FinishReason::Stop
    );
    flaky2.clock.assert_waits(&[]);
    assert_eq!(
        std::fs::read_to_string(fx2.dir.join("count.txt")).unwrap(),
        "ran\n"
    );
    assert_eq!(flaky2.requests.lock().unwrap().len(), 2);
    assert!(
        !fx2.log()
            .await
            .iter()
            .any(|e| matches!(e, Event::PartDeleted { .. }))
    );
}

#[tokio::test]
async fn cancel_during_retry_wait_ends_run_with_parts_deleted() {
    let (fx, flaky) = flaky_fixture(
        "retry-cancelled-wait",
        vec![text("unreached")],
        vec![Attempt::Stream(
            vec![
                FakeStep::Reasoning("half a thought".into()),
                FakeStep::Text("partial answer".into()),
            ],
            server_error("retryable"),
        )],
    )
    .await;
    let cancel = CancellationToken::new();
    let publish = |envelope: Envelope| {
        if matches!(envelope.event, Event::PartDeleted { .. }) {
            cancel.cancel();
        }
    };
    let error = fx
        .turn_with_publish("hi", &cancel, &publish)
        .await
        .expect_err("cancelled wait ends run");
    flaky.clock.finish_wait();
    assert!(matches!(error, CoreError::Cancelled));
    assert_eq!(flaky.requests.lock().unwrap().len(), 1);
    assert_eq!(
        fx.log()
            .await
            .iter()
            .filter(|e| matches!(e, Event::PartDeleted { .. }))
            .count(),
        2
    );
    let projection = fx.store.read_projection(fx.session).await.unwrap();
    assert!(projection.session.messages.last().unwrap().parts.is_empty());
}

// ============================================================================
// 4. Builtin read, edit, and image attachments
// ============================================================================

#[tokio::test]
async fn read_image_reaches_model_once_live_and_replayed_and_handles_oversized() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let mut looked = text("A board.");
    looked.extend(call("todo", json!({"ops": []})));
    let fx = fixture(
        "read-image",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.data"})),
            call("todo", json!({"ops": []})),
            looked,
            text("done"),
        ]),
    )
    .await;
    std::fs::write(
        fx.dir.join("board.data"),
        STANDARD.decode(BOARD_PNG).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fx.turn("look at the board", &CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert!(requests.len() >= 4);
    let wire: Vec<serde_json::Value> = requests
        .iter()
        .map(|req| {
            AnthropicMessagesProtocol
                .encode_for_model(req, None, true)
                .unwrap()
        })
        .collect();
    let result = &wire[1]["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["is_error"], false);
    assert_eq!(
        result["content"][1],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": BOARD_PNG}})
    );
    for later in &wire[2..] {
        let s = later.to_string();
        assert!(!s.contains(BOARD_PNG));
        assert!(s.contains("Image file (image/png, 80 bytes).\\n\\n[An image is sent to you only"));
    }
    let envelopes = fx.store.replay(fx.session).await.unwrap();
    for (step, sent) in wire.iter().enumerate().skip(1) {
        let started = envelopes
            .iter()
            .position(
                |e| matches!(e.event, Event::StepStarted { step: s, .. } if s as usize == step),
            )
            .unwrap();
        let projection = Projection::from_events(&envelopes[..started]);
        let rebuilt = kernel::request::request(
            &View {
                projection: &projection,
            },
            requests[step].system.as_deref().unwrap(),
            requests[step].tools.clone(),
            &fx.model,
            &AgentName::new(hya_engine::BARE_AGENT_ID),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            AnthropicMessagesProtocol
                .encode_for_model(&rebuilt, None, true)
                .unwrap(),
            *sent
        );
    }

    // Oversized image returns resize guidance without attachment.
    let over = fixture(
        "read-oversized-image",
        FakeProvider::scripted_turns(vec![call("read", json!({"path": "big.png"})), text("done")]),
    )
    .await;
    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
    big.resize(1_572_814, 0);
    std::fs::write(over.dir.join("big.png"), big).unwrap();
    assert_eq!(
        over.turn("look", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let reqs = over.provider.requests();
    let output = assistant_parts(&reqs[1])
        .find_map(|p| match p {
            Part::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "read" => Some(output),
            _ => None,
        })
        .unwrap();
    assert!(output.get("attachments").is_none());
    let notice = output["output"].as_str().unwrap();
    assert!(notice.contains("1572814 bytes"));
    assert!(notice.contains("1572813 bytes"));
    assert!(notice.contains("Downscale or crop"));
}

#[tokio::test]
async fn large_image_result_stays_below_rewrite_pressure() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let fx = fixture(
        "large-image-pressure",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "board.png"})),
            text("done"),
            text("checked"),
        ])
        .with_max_context(9_000)
        .with_tool_images(),
    )
    .await;
    // Preserve a realistic large transport payload with a valid PNG prefix.
    let mut png = STANDARD.decode(BOARD_PNG).unwrap();
    png.resize(384 * 1024, 0);
    std::fs::write(fx.dir.join("board.png"), png).unwrap();

    assert_eq!(
        fx.turn("look", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        [
            goal_checkpoints(&requests[0]),
            goal_checkpoints(&requests[1])
        ],
        [0, 1]
    );
    assert!(assistant_parts(&requests[1]).any(|part| matches!(
        part,
        Part::Tool { state: ToolPartState::Completed { output, .. }, .. }
        if output["attachments"][0]["url"].as_str().is_some_and(|url| url.len() > 500_000)
    )));
    assert!(compactions(&fx.log().await).is_empty());
}

// ============================================================================
// 5. Builtin bash: cancel, background/yield, budget share/cap, shutdown, output
// ============================================================================

struct PidFile(PathBuf);

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0)
            && let Ok(pid) = pid.trim().parse::<libc::pid_t>()
            && pid > 0
        {
            // SAFETY: positive PID belongs to the test's command.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

struct LeftRunning {
    go: PathBuf,
    pgid: Option<libc::pid_t>,
    dir: Option<PathBuf>,
}

impl LeftRunning {
    fn new(workdir: &Path) -> Self {
        Self {
            go: workdir.join("go"),
            pgid: None,
            dir: None,
        }
    }
}

impl Drop for LeftRunning {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.go, "");
        if let Some(pgid) = self.pgid.filter(|pgid| *pgid > 0) {
            // SAFETY: positive group ID belongs to the test's command.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

async fn assert_bash_yield(
    result: &serde_json::Value,
    left: &mut LeftRunning,
    expected_wait: &str,
) {
    let note = result["still_running"].as_str().unwrap_or_default();
    left.pgid = note
        .split_once("process group ")
        .and_then(|(_, rest)| rest.split_once(';'))
        .and_then(|(pgid, _)| pgid.parse().ok());
    left.dir = note
        .split_once("goes to '")
        .and_then(|(_, rest)| rest.split_once("/output'"))
        .map(|(dir, _)| PathBuf::from(dir));

    assert_eq!(result["exit_code"], serde_json::Value::Null);
    assert!(result.get("timed_out").is_none(), "{result}");
    let model_text = tool_result_text(result);
    let status = format!("[still running: {note}]");
    assert_eq!(model_text.lines().last(), Some(status.as_str()));
    let output = model_text.strip_suffix(&status).unwrap();
    assert!("first\n".starts_with(output), "{output:?}");
    assert!(note.contains(&format!("waited {expected_wait}")), "{note}");
    assert!(note.contains("was not stopped"), "{note}");
    let pgid = left.pgid.unwrap_or_else(|| panic!("no pgid in {note}"));
    let dir = left
        .dir
        .clone()
        .unwrap_or_else(|| panic!("no output dir in {note}"));
    let (out, exit) = (dir.join("output"), dir.join("exit_code"));
    assert!(note.contains(&format!("`tail -n 20 '{}'`", out.display())));
    assert!(note.contains(&format!("`cat '{}'`", exit.display())));
    assert!(note.contains(&format!("`kill -- -{pgid}`")));
    // SAFETY: signal 0 only probes group existence.
    assert_eq!(unsafe { libc::killpg(pgid, 0) }, 0, "group was stopped");

    std::fs::write(&left.go, "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !exit.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("exit code written");
    left.pgid = None;
    assert_eq!(std::fs::read_to_string(&exit).unwrap(), "7\n");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "first\nsecond\n");
    assert_eq!(
        std::fs::read_to_string(left.go.with_file_name("pgid"))
            .unwrap()
            .trim(),
        pgid.to_string()
    );
}

#[tokio::test]
async fn a3_cancel_is_an_observation() {
    let mut round = call("bash", json!({"command": "sleep 30 & echo $! > pid; wait"}));
    round.extend(tool_round("bash", json!({"command": "touch unrun"})));
    let fx = fixture("a3", FakeProvider::scripted_turns(vec![round])).await;
    let cancel = CancellationToken::new();
    let pid_file = fx.dir.join("pid");
    let _sleep = PidFile(pid_file.clone());
    let canceller = tokio::spawn({
        let cancel = cancel.clone();
        let pid_file = pid_file.clone();
        async move {
            while !std::fs::read_to_string(&pid_file).is_ok_and(|pid| pid.ends_with('\n')) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            cancel.cancel();
        }
    });
    let finish = tokio::time::timeout(Duration::from_secs(10), fx.turn("sleep", &cancel))
        .await
        .expect("cancel ends the run");
    canceller.await.unwrap();
    assert_eq!(finish.unwrap(), FinishReason::Cancelled);
    let log = fx.log().await;
    assert_paired(&log);
    assert_eq!(
        tool_errors(&log),
        vec![
            "cancelled",
            "cancelled: the turn stopped before this tool call finished"
        ]
    );
    assert!(!fx.dir.join("unrun").exists());
    assert!(matches!(
        tool_event_order(&log).as_slice(),
        [Event::ToolInputStart { call, .. },
         Event::ToolCallRequested { call: request, .. },
         Event::ToolInputStart { call: pending, .. },
         Event::ToolCallRequested { call: pending_request, .. },
         Event::StepFinished { step: 0, .. },
         Event::ToolError { call: result, value: Some(value), .. },
         Event::ToolError { call: closed, value: Some(close_value), .. }]
            if call == request && call == result && value["error"]["type"] == "cancelled"
                && pending == pending_request && pending == closed && close_value["code"] == "CANCELLED"
    ));
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let pid: libc::pid_t = pid.trim().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // SAFETY: signal 0 only checks process liveness.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.contains(") Z "))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child process terminated");
}

#[tokio::test]
async fn bash_syntax_exit_codes_and_pythonunbuffered() {
    let bash = builtin_tool("bash");
    let ctx = tool_ctx(Path::new("."), &CancellationToken::new());
    for (command, code) in [("true", 0), ("kill -TERM $$", 143)] {
        let result = bash
            .execute(&ctx, json!({"command": command}))
            .await
            .unwrap();
        assert_eq!(result["exit_code"], code);
        assert_eq!(tool_result_text(&result), format!("[exit code: {code}]"));
    }
    let unbuf = bash
        .execute(
            &ctx,
            json!({"command": r#"printf '%s' "$PYTHONUNBUFFERED""#}),
        )
        .await
        .unwrap();
    assert_eq!(unbuf["output"], "1\n[exit code: 0]");

    let bash_available = std::process::Command::new("bash")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let syntax = bash
        .execute(
            &ctx,
            json!({"command": r#"
                printf 'shell\n'
                if [ -n "${BASH_VERSION-}" ]; then
                    eval '[[ word == w* ]] && printf "bash\n"'
                fi
                false | true
            "#}),
        )
        .await
        .unwrap();
    assert_eq!(syntax["exit_code"], 0);
    if bash_available {
        assert_eq!(syntax["output"], "shell\nbash\n[exit code: 0]");
    }
}

#[tokio::test]
async fn bash_background_and_timeout_yield() {
    // 1. Plain `&` job outlives shell and timeout=0 is rejected as input error.
    let fx = fixture(
        "bash-job",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({"command": "(while [ ! -e go ]; do sleep 0.01; done; echo done > marker) & echo $! > job; echo started; exit 3"}),
            ),
            call("bash", json!({"command": "true", "timeout": 0})),
            text("ok"),
        ]),
    )
    .await;
    let job = PidFile(fx.dir.join("job"));
    assert_eq!(
        fx.turn("run", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let log = fx.log().await;
    assert_paired(&log);
    let results: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["exit_code"], 3);
    assert_eq!(tool_result_text(results[0]), "started\n[exit code: 3]");
    assert_eq!(tool_errors(&log).len(), 1);
    let pid: libc::pid_t = std::fs::read_to_string(&job.0)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 probes process existence.
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "job stayed alive");
    std::fs::write(fx.dir.join("go"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while std::fs::read_to_string(fx.dir.join("marker")).unwrap_or_default() != "done\n" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background job finished");

    // 2. Explicit timeout yields a running process group (using paused clock to avoid 1 s wait).
    let dir = super::support::TestDir::new("kernel-tool");
    let mut left = LeftRunning::new(dir.path());
    let bash = builtin_tool("bash");
    let ctx = tool_ctx(dir.path(), &CancellationToken::new());
    tokio::time::pause();
    let result = bash
        .execute(&ctx, json!({"command": BASH_YIELD_COMMAND, "timeout": 1}))
        .await
        .unwrap();
    tokio::time::resume();
    assert_bash_yield(&result, &mut left, "1s (the `timeout` this call passed)").await;
}

#[tokio::test]
async fn bash_waits_at_most_a_tenth_of_the_run_budget() {
    let bash = builtin_tool("bash");
    for (budget, requested, waited) in [
        (20u64, Some(3u64), "2s"),
        (900, None, "90s"),
        (1800, Some(600), "180s"),
        (3600, None, "300s"),
        (3600, Some(600), "360s"),
    ] {
        let dir = super::support::TestDir::new("kernel-tool");
        let mut left = LeftRunning::new(dir.path());
        let ctx = tool_ctx(dir.path(), &CancellationToken::new());
        let mut input = json!({"command": BASH_YIELD_COMMAND});
        if let Some(seconds) = requested {
            input["timeout"] = json!(seconds);
        }
        let reason = if waited == "300s" {
            "the default; pass `timeout` in seconds to request a longer wait"
        } else {
            "the run's time budget lets one call wait at most a tenth of it"
        };
        let _deadline = deadline::install_scoped(Duration::from_secs(budget));
        tokio::time::pause();
        let result = bash.execute(&ctx, input).await.unwrap();
        tokio::time::resume();
        assert_bash_yield(&result, &mut left, &format!("{waited} ({reason})")).await;
    }
}

#[tokio::test]
async fn bash_budget_cap_kills_group_and_refuses_zero_remaining() {
    let fx = fixture(
        "bash-budget-cap",
        FakeProvider::scripted_turns(vec![
            call(
                "bash",
                json!({"command": "sleep 30 & echo $! > pid; echo started; wait"}),
            ),
            text("done"),
        ]),
    )
    .await;
    let job = PidFile(fx.dir.join("pid"));
    // Start 4.75 s into a 5 s budget (~250 ms left) so the end-of-budget cap fires fast.
    let deadline =
        deadline::install_scoped_with_elapsed(Duration::from_secs(5), Duration::from_millis(4750));
    let finish = tokio::time::timeout(
        Duration::from_secs(10),
        fx.turn("run", &CancellationToken::new()),
    )
    .await
    .expect("capped call returned")
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let log = fx.log().await;
    assert_paired(&log);
    let result = log
        .iter()
        .find_map(|e| match e {
            Event::ToolResult { output, .. } => Some(output),
            _ => None,
        })
        .unwrap();
    assert_eq!(result["exit_code"], serde_json::Value::Null);
    assert!(result.get("still_running").is_none());
    let note = result["timed_out"].as_str().unwrap();
    assert!(note.contains("the run's time budget capped the timeout"));
    assert!(note.contains("killed its process group"));

    // At 0 remaining, commands are refused without starting.
    deadline.advance_clock(Duration::from_secs(1));
    let bash = builtin_tool("bash");
    let ctx = tool_ctx(&fx.dir, &CancellationToken::new());
    let refused = bash
        .execute(&ctx, json!({"command": "touch unexpected"}))
        .await
        .unwrap();
    assert!(!fx.dir.join("unexpected").exists());
    assert_eq!(refused["exit_code"], serde_json::Value::Null);
    assert_eq!(
        tool_result_text(&refused),
        format!("[not started: {}]", refused["timed_out"].as_str().unwrap())
    );

    if let Ok(raw) = std::fs::read_to_string(&job.0)
        && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
    {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                // SAFETY: signal 0 probes liveness.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                #[cfg(target_os = "linux")]
                if std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .is_ok_and(|s| s.contains(") Z "))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("capped command's child terminated");
    }
}

#[tokio::test]
async fn budgeted_bash_detached_writer_and_worker_thread_lifetime() {
    // 1. Detached setsid writer does not delay budgeted Bash completion.
    {
        let dir = super::support::TestDir::new("kernel-tool");
        let daemon = PidFile(dir.path().join("daemon.pid"));
        let _deadline = deadline::install_scoped(Duration::from_secs(180));
        let bash = builtin_tool("bash");
        let ctx = tool_ctx(dir.path(), &CancellationToken::new());
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            bash.execute(
                &ctx,
                json!({
                    "command": "python3 -c 'import os,time; os.setsid(); time.sleep(30)' & echo $! > daemon.pid",
                    "timeout": 10
                }),
            ),
        )
        .await
        .expect("detached writer delayed Bash")
        .unwrap();
        assert_eq!(result["exit_code"], 0, "{result}");
        let pid: libc::pid_t = std::fs::read_to_string(&daemon.0)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: query session of test's background process.
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
    }

    // 2. Linux: worker threads & fork handoffs stay tracked in bare & full bash.
    #[cfg(target_os = "linux")]
    {
        let dir = super::support::TestDir::new("kernel-tool");
        let build = std::process::Command::new("cc")
            .args(["-pthread", "-o", "worker"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/worker_thread.c"
            ))
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );

        let _deadline = deadline::install_scoped(Duration::from_secs(180));
        let bare_bash = builtin_tool("bash");
        let pid_file = dir.path().join("worker-ids");

        for (case, bash) in [("bare", bare_bash)] {
            for cancel in [false, true] {
                let token = CancellationToken::new();
                let mut ctx = tool_ctx(dir.path(), &token);
                ctx.permission = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
                    Action::Bash,
                    "*",
                    Mode::Allow,
                )]))
                .0;
                let call = bash.execute(
                    &ctx,
                    json!({"command": "( (./worker & exit) & exit) & exit 7"}),
                );
                let observe = async {
                    let (pid, tid) = loop {
                        if let Ok(ids) = std::fs::read_to_string(&pid_file)
                            && let Some((pid, tid)) = ids.split_once(' ')
                            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                                .is_ok_and(|s| s.contains(") Z "))
                        {
                            break (pid.to_owned(), tid.to_owned());
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    };
                    let worker = PathBuf::from(format!("/proc/{pid}/task/{tid}"));
                    assert!(worker.exists());
                    if cancel {
                        token.cancel();
                    } else {
                        std::fs::write(dir.path().join("go"), "").unwrap();
                    }
                    worker
                };
                let (result, worker) = tokio::time::timeout(Duration::from_secs(10), async {
                    tokio::join!(call, observe)
                })
                .await
                .expect("group completion");
                assert!(!worker.exists(), "{case}: worker still present");
                if cancel {
                    assert!(
                        matches!(result, Err(ToolError::Cancelled)),
                        "{case}: {result:?}"
                    );
                } else {
                    let result = result.unwrap();
                    assert!(
                        result["output"]
                            .as_str()
                            .unwrap()
                            .contains("worker finished")
                    );
                    let exit = if case == "full" {
                        &result["metadata"]["exit"]
                    } else {
                        &result["exit_code"]
                    };
                    assert_eq!(exit, 7, "{case}");
                    std::fs::remove_file(dir.path().join("go")).unwrap();
                }
                std::fs::remove_file(&pid_file).unwrap();
            }
        }
    }
}

#[tokio::test]
async fn bash_shutdown_awaits_admitted_jobs_and_rejects_late_spawns() {
    use hya_tool::process_group::{BudgetGroup, GroupOutcome};
    use tokio::{process::Command, time::sleep};

    // Budgeted case runs in-process on its own ScopedDeadline.
    {
        let dir = super::support::TestDir::new("kernel-tool");
        let deadline = deadline::install_scoped(Duration::from_secs(180));
        let cancel = CancellationToken::new();
        let exit = dir.path().join("exit_code");
        let run = |command, wait| {
            BudgetGroup::run(
                command,
                deadline::current(),
                &cancel,
                sleep(wait),
                false,
                exit.clone(),
            )
        };
        let sleeper = || {
            let mut cmd = Command::new("sleep");
            cmd.arg("60");
            cmd
        };
        let Ok(GroupOutcome::Yielded(pgid)) = run(sleeper(), Duration::ZERO).await else {
            panic!("expected yielded group");
        };
        let mut foreground = std::pin::pin!(run(sleeper(), Duration::from_secs(60)));
        assert!(futures::poll!(&mut foreground).is_pending());
        let mut first_drain = Box::pin(deadline.shutdown_background_jobs());
        assert!(futures::poll!(&mut first_drain).is_pending());
        drop(first_drain);
        assert!(matches!(
            run(Command::new("nonexistent-late-command"), Duration::ZERO).await,
            Err(ToolError::Cancelled)
        ));
        tokio::time::timeout(Duration::from_secs(5), deadline.shutdown_background_jobs())
            .await
            .expect("shutdown completed");
        assert_eq!(std::fs::read_to_string(&exit).unwrap(), "137\n");
        // SAFETY: signal 0 probes group existence.
        assert_eq!(unsafe { libc::killpg(pgid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(matches!(foreground.await, Err(ToolError::Cancelled)));
    }

    // Global unbudgeted shutdown closes process-wide admission (isolated subprocess).
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "kernel::bash_unbudgeted_shutdown_child",
            "--nocapture",
        ])
        .env("HYA_BASH_UNBUDGETED_SHUTDOWN", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn bash_unbudgeted_shutdown_child() {
    use hya_tool::process_group::{BudgetGroup, GroupOutcome};
    use tokio::{process::Command, time::sleep};

    if std::env::var_os("HYA_BASH_UNBUDGETED_SHUTDOWN").is_none() {
        return;
    }
    let dir = super::support::TestDir::new("kernel-tool");
    let cancel = CancellationToken::new();
    let exit = dir.path().join("exit_code");
    let run =
        |command, wait| BudgetGroup::run(command, None, &cancel, sleep(wait), false, exit.clone());
    let sleeper = || {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        cmd
    };
    let Ok(GroupOutcome::Yielded(pgid)) = run(sleeper(), Duration::ZERO).await else {
        panic!("expected yielded group");
    };
    let mut foreground = std::pin::pin!(run(sleeper(), Duration::from_secs(60)));
    assert!(futures::poll!(&mut foreground).is_pending());
    let mut first_drain = Box::pin(deadline::shutdown_background_jobs());
    assert!(futures::poll!(&mut first_drain).is_pending());
    drop(first_drain);
    assert!(matches!(
        run(Command::new("nonexistent-late-command"), Duration::ZERO).await,
        Err(ToolError::Cancelled)
    ));
    tokio::time::timeout(Duration::from_secs(5), deadline::shutdown_background_jobs())
        .await
        .expect("shutdown completed");
    assert_eq!(std::fs::read_to_string(&exit).unwrap(), "137\n");
    // SAFETY: signal 0 probes group existence.
    assert_eq!(unsafe { libc::killpg(pgid as libc::pid_t, 0) }, -1);
    assert!(matches!(foreground.await, Err(ToolError::Cancelled)));
}

#[tokio::test]
async fn bash_output_limits_and_large_yield() {
    let bash = builtin_tool("bash");
    for (size, yields) in [(30_000usize, false), (50_000, false), (50_000, true)] {
        let dir = super::support::TestDir::new("kernel-tool");
        let mut left = LeftRunning::new(dir.path());
        let ctx = tool_ctx(dir.path(), &CancellationToken::new());
        let padding = size - "first\nlast\n".len();
        let mut command = format!(
            r#"printf 'first\n'; head -c {padding} /dev/zero | tr '\000' x; printf 'last\n' >&2"#
        );
        if yields {
            command.push_str("; echo $$ > ready; while [ ! -e go ]; do sleep 0.01; done");
        }
        let call = bash.execute(&ctx, json!({"command": command, "timeout": 300}));
        tokio::pin!(call);
        let result = if yields {
            let ready = dir.path().join("ready");
            tokio::select! {
                result = &mut call => panic!("returned before ready: {result:?}"),
                () = async {
                    tokio::time::timeout(Duration::from_secs(10), async {
                        loop {
                            if let Ok(pid) = std::fs::read_to_string(&ready)
                                && let Ok(pid) = pid.trim().parse()
                            {
                                left.pgid = Some(pid);
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    })
                    .await
                    .expect("ready signal");
                } => {}
            }
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(300)).await;
            let res = call.await.unwrap();
            tokio::time::resume();
            res
        } else {
            tokio::time::timeout(Duration::from_secs(10), call)
                .await
                .expect("command ended")
                .unwrap()
        };

        let model_text = tool_result_text(&result);
        let status = if yields {
            format!(
                "[still running: {}]",
                result["still_running"].as_str().unwrap()
            )
        } else {
            "[exit code: 0]".to_string()
        };
        let output = model_text.strip_suffix(&status).unwrap();
        let expected = format!("first\n{}last\n", "x".repeat(padding));
        if size == 30_000 {
            assert_eq!(result["exit_code"], 0);
            assert_eq!(output, expected);
            continue;
        }
        let marker = output
            .lines()
            .find(|line| line.starts_with("[… "))
            .expect("omission marker");
        let (_, rest) = marker.split_once(" bytes are in '").unwrap();
        let (path, _) = rest.split_once("'.").unwrap();
        let path = PathBuf::from(path);
        left.dir = path.parent().map(Path::to_path_buf);

        assert!(marker.starts_with("[… 20000 bytes omitted here. All 50000 bytes"));
        assert_eq!(output.len(), 30_000 + marker.len() + 2);
        assert!(output.starts_with(&expected[..10_000]));
        assert!(output.ends_with(&expected[size - 20_000..]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        if yields {
            assert_eq!(result["exit_code"], serde_json::Value::Null);
            assert!(
                result["still_running"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("goes to '{}'.", path.display()))
            );
        } else {
            assert_eq!(result["exit_code"], 0);
        }
    }
}

// ============================================================================
// 6. Rewrite, compaction extensions, goal checkpoint, and todos
// ============================================================================

#[tokio::test]
async fn a4_a5_model_rewrite_and_nothing_to_remove() {
    // A4: model rewrite removes prior turns while preserving summary & log.
    let fx = fixture(
        "a4",
        FakeProvider::scripted_turns(vec![
            text("hi"),
            call("bash", json!({"command": "echo one"})),
            call("bash", json!({"command": "echo two"})),
            call("rewrite", json!({"summary": "ran two echoes"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    let cancel = CancellationToken::new();
    fx.turn("first task: greet", &cancel).await.unwrap();
    assert_eq!(
        fx.turn("second task: echo", &cancel).await.unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 5);
    let next = &requests[4];
    let tools: Vec<_> = assistant_parts(next)
        .filter_map(|p| match p {
            Part::Tool { name, state, .. } => Some((name.as_str(), state)),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0, "rewrite");
    let ToolPartState::Completed { input, output, .. } = tools[0].1 else {
        panic!("rewrite did not complete");
    };
    assert_eq!(input, &json!({"summary": "ran two echoes"}));
    assert_eq!(output["removed_messages"], 3);
    let marker = next
        .messages
        .iter()
        .find_map(|m| match m {
            Message::System { content, .. } if content.starts_with(COMPACT_CONTEXT_MARKER) => {
                Some(content)
            }
            _ => None,
        })
        .expect("marker message");
    assert!(marker.contains("first task: greet") && marker.contains("removed by the model"));
    let log = fx.log().await;
    assert_paired(&log);
    assert!(log.iter().any(|e| matches!(
        e,
        Event::ContextCompacted {
            strategy: CompactionStrategy::Rewrite,
            step_cut: Some(_),
            ..
        }
    )));

    // A5: rewrite with nothing to remove fails cleanly without compaction.
    let fx2 = fixture(
        "a5",
        FakeProvider::scripted_turns(vec![
            call("rewrite", json!({"summary": "nothing yet"})),
            text("ok"),
        ]),
    )
    .await;
    assert_eq!(
        fx2.turn("start", &cancel).await.unwrap(),
        FinishReason::Stop
    );
    let log2 = fx2.log().await;
    assert_paired(&log2);
    assert_eq!(tool_errors(&log2), vec!["nothing to remove"]);
    assert!(
        !log2
            .iter()
            .any(|e| matches!(e, Event::ContextCompacted { .. }))
    );
}

fn round_at(tokens: u64) -> Vec<FakeStep> {
    vec![
        FakeStep::ToolCall {
            name: "bash".to_string(),
            input: json!({"command": "echo hi"}),
        },
        FakeStep::Usage(TokenUsage {
            input: tokens,
            ..TokenUsage::default()
        }),
        FakeStep::Finish(FinishReason::ToolCalls),
    ]
}

fn goal_checkpoints(request: &CompletionRequest) -> usize {
    request
        .messages
        .iter()
        .filter(|m| matches!(m, Message::System { content, .. } if content.starts_with("[hya] Goal checkpoint:")))
        .count()
}

fn compactions(log: &[Event]) -> Vec<CompactionStrategy> {
    log.iter()
        .filter_map(|e| match e {
            Event::ContextCompacted { strategy, .. } => Some(*strategy),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn goal_checkpoint_asked_once_and_stays_out_after_rewrite() {
    // Below 60%: none; at 65%: asked once; at 70%: not repeated.
    let fx = fixture(
        "goal-checkpoint",
        FakeProvider::scripted_turns(vec![
            round_at(2_000),
            round_at(13_000),
            round_at(14_000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    assert_eq!(
        fx.turn("go", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        fx.provider
            .requests()
            .iter()
            .map(goal_checkpoints)
            .collect::<Vec<_>>(),
        [0, 0, 1, 0]
    );
    assert!(compactions(&fx.log().await).is_empty());

    // After rewrite crosses 80%, checkpoint still stays out.
    let fx2 = fixture(
        "goal-checkpoint-rewrite",
        FakeProvider::scripted_turns(vec![
            round_at(2_000),
            round_at(13_000),
            round_at(17_000),
            round_at(14_000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    assert_eq!(
        fx2.turn("go", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let reqs = fx2.provider.requests();
    assert_eq!(
        reqs.iter().map(goal_checkpoints).collect::<Vec<_>>(),
        [0, 0, 1, 0, 0]
    );
    assert!(reqs[3].messages.iter().any(|m| matches!(
        m,
        Message::System { content, .. } if content.starts_with("HYA_COMPACTED_CONTEXT")
    )));
}

struct Probe {
    fold: bool,
    seen: Mutex<Vec<Option<Pressure>>>,
}

#[async_trait::async_trait]
impl Rewriter for Probe {
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        self.seen.lock().unwrap().push(cx.pressure);
        if !cx.pressure.is_some_and(Pressure::over) {
            return Ok(false);
        }
        let session = cx.session;
        let messages = &cx.projection.session.messages;
        let head = messages.iter().rposition(|m| m.role == Role::User).unwrap();
        let (marker, part) = (MessageId::new(), PartId::new());
        let mut events = vec![
            Event::MessageStarted {
                session,
                message: marker,
                role: Role::System,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session,
                message: marker,
                part,
            },
            Event::TextDelta {
                session,
                message: marker,
                part,
                delta: if self.fold {
                    format!("{COMPACT_CONTEXT_MARKER}\nfolded by the probe")
                } else {
                    "a note from the probe".to_string()
                },
            },
            Event::TextEnd {
                session,
                message: marker,
                part,
            },
            Event::MessageFinished {
                session,
                message: marker,
                role: Role::System,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        if self.fold {
            events.push(Event::ContextCompacted {
                session,
                message: marker,
                strategy: CompactionStrategy::Native,
                from_message: messages[0].id,
                to_message: messages[head - 1].id,
                folded_count: u32::try_from(head).unwrap(),
                input_tokens_est: 0,
                threshold: 0,
                step_cut: None,
            });
        }
        cx.log.append(session, events).await?;
        Ok(true)
    }

    async fn rewrite_now(&self, _cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        Ok(false)
    }
}

struct StoreLog<'a>(&'a SessionStore);

#[async_trait::async_trait]
impl SessionLog for StoreLog<'_> {
    async fn projection(&self, session: SessionId) -> Result<Arc<Projection>, ToolError> {
        Ok(self.0.read_projection_shared(session).await.unwrap())
    }

    async fn append(&self, session: SessionId, events: Vec<Event>) -> Result<(), ToolError> {
        self.0.append_events(session, events).await.unwrap();
        Ok(())
    }
}

struct KernelTranscript(ModelRef);

#[async_trait::async_trait]
impl Transcript for KernelTranscript {
    async fn messages(&self, projection: &Projection) -> Result<Vec<Message>, ToolError> {
        Ok(kernel::request::projection_to_messages(
            &AgentName::new(hya_engine::BARE_AGENT_ID),
            projection,
            &self.0,
            &AttachmentData::new(),
        ))
    }
}

#[tokio::test]
async fn before_round_rewriter_extensions_spare_or_fall_through_to_kernel_rewrite() {
    for (name, fold) in [("extension-enough", true), ("extension-short", false)] {
        let fx = fixture(
            name,
            FakeProvider::scripted_turns(vec![text(&"earlier detail ".repeat(5_000))]),
        )
        .await;
        fx.turn("first", &CancellationToken::new()).await.unwrap();
        let message = fx.begin("second").await;
        let projection = fx.store.read_projection_shared(fx.session).await.unwrap();
        let log = StoreLog(&fx.store);
        let transcript = KernelTranscript(fx.model.clone());
        let probe = Arc::new(Probe {
            fold,
            seen: Default::default(),
        });
        let rewriter: Arc<dyn hya_context::rewrite::Rewriter> = probe.clone();
        let changed = kernel::before_round(
            RewriteCtx {
                session: fx.session,
                message,
                model: &fx.model,
                projection: &projection,
                window: Some(ModelWindow {
                    max_context: 10_000,
                    usage_reporting: false,
                    reasoning_replay: hya_proto::context::ReasoningReplayPolicy::None,
                    sends_tool_images: false,
                }),
                overhead_tokens: 0,
                pressure: None,
                workdir: &fx.dir,
                config: &serde_json::Value::Null,
                log: &log,
                transcript: &transcript,
                models: None,
                hooks: None,
            },
            std::slice::from_ref(&rewriter),
        )
        .await
        .unwrap();
        assert!(changed, "{name}");
        let seen = probe.seen.lock().unwrap().clone();
        let pressure = seen[0].expect("pressure");
        assert_eq!(pressure.threshold, 8_000);
        assert!(pressure.over());
        let events = fx.log().await;
        if fold {
            assert_eq!(compactions(&events), [CompactionStrategy::Native]);
        } else {
            assert_eq!(compactions(&events), [CompactionStrategy::Rewrite]);
            let probe_pos = events
                .iter()
                .position(|e| matches!(e, Event::TextDelta { delta, .. } if delta == "a note from the probe"))
                .unwrap();
            let rewrite_pos = events
                .iter()
                .position(|e| matches!(e, Event::ContextCompacted { .. }))
                .unwrap();
            assert!(probe_pos < rewrite_pos);
        }
    }
}

#[tokio::test]
async fn a6_harness_rewrite_and_restart() {
    let fx = fixture(
        "a6",
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::Reasoning("look before acting".into()),
                FakeStep::ToolCall {
                    name: "bash".into(),
                    input: json!({"command": "echo hi"}),
                },
                FakeStep::Usage(TokenUsage {
                    input: 3_600,
                    output: 20,
                    ..TokenUsage::default()
                }),
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            text("done"),
            text("checked"),
        ])
        .with_max_context(4_000),
    )
    .await;
    assert_eq!(
        fx.turn("go", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let envelopes = fx.store.replay(fx.session).await.unwrap();
    let log: Vec<_> = envelopes.iter().map(|e| e.event.clone()).collect();
    assert_paired(&log);

    let compacted = log
        .iter()
        .position(|e| {
            matches!(
                e,
                Event::ContextCompacted {
                    strategy: CompactionStrategy::Rewrite,
                    ..
                }
            )
        })
        .expect("harness rewrite");
    let next_step = log
        .iter()
        .position(|e| matches!(e, Event::StepStarted { step: 1, .. }))
        .expect("second round");
    assert!(compacted < next_step);
    let Event::MessageStarted {
        message: marker, ..
    } = &log[compacted - 5]
    else {
        panic!("marker message precedes compaction");
    };
    assert!(
        matches!(&log[compacted], Event::ContextCompacted { message, .. } if message == marker)
    );

    let reasoning = log
        .iter()
        .find_map(|e| match e {
            Event::ReasoningStart { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    let call_part = log
        .iter()
        .find_map(|e| match e {
            Event::ToolCallRequested { part, .. } => Some(*part),
            _ => None,
        })
        .unwrap();
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 2);
    let sent = part_ids(&requests[1]);
    assert_eq!(sent.contains(&reasoning), sent.contains(&call_part));

    let mut attempts = 0;
    for (index, envelope) in envelopes.iter().enumerate() {
        let Event::StepStarted { view, .. } = &envelope.event else {
            continue;
        };
        let recorded = view.as_ref().expect("step has view");
        assert_eq!(recorded.cut, envelopes[index - 1].seq);
        let end = envelopes.partition_point(|entry| entry.seq <= recorded.cut);
        let projection = Projection::from_events(&envelopes[..end]);
        let rebuilt = kernel::request::request(
            &View {
                projection: &projection,
            },
            requests[attempts].system.as_deref().unwrap(),
            requests[attempts].tools.clone(),
            &fx.model,
            &AgentName::new(hya_engine::BARE_AGENT_ID),
            &AttachmentData::new(),
            None,
        );
        assert_eq!(
            kernel::request::request_sha256(&rebuilt).unwrap(),
            recorded.request_sha256
        );
        assert_eq!(format!("{rebuilt:?}"), format!("{:?}", requests[attempts]));
        attempts += 1;
    }
    assert_eq!(attempts, requests.len());
}

#[tokio::test]
async fn todo_rewrite_carryover_rollback_and_id_monotonicity() {
    let cancel = CancellationToken::new();

    // 1. Rewrite carries latest todo result verbatim.
    let fx = fixture(
        "rewrite-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [
                    {"op": "add", "content": "inspect the inputs"},
                    {"op": "add", "content": "check all outputs"},
                    {"op": "status", "id": "2", "status": "in_progress"}
                ]}),
            ),
            call("rewrite", json!({"summary": "continue checking"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx.turn("check", &cancel).await.unwrap();
    let log = fx.log().await;
    assert_paired(&log);
    let list =
        "Current todos:\n- #1 [pending] inspect the inputs\n- #2 [in_progress] check all outputs";
    assert!(log.windows(2).any(|pair| matches!(
        pair,
        [Event::ToolResult { output, .. }, Event::TodosUpdated { todos, .. }]
            if output == list && todos.len() == 2
    )));
    let reqs = fx.provider.requests();
    assert!(
        !assistant_parts(&reqs[2])
            .any(|p| matches!(p, Part::Tool { name, .. } if name.as_str() == "todo"))
    );
    assert!(reqs[2].messages.iter().any(|m| matches!(
        m,
        Message::System { content, .. } if content.starts_with(COMPACT_CONTEXT_MARKER) && content.ends_with(list)
    )));

    // 2. Invalid batch rolls back atomically without TodosUpdated.
    let fx2 = fixture(
        "invalid-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [{"op": "add", "content": "keep me"}]}),
            ),
            call(
                "todo",
                json!({"ops": [
                    {"op": "edit", "id": "1", "content": "undo this"},
                    {"op": "add", "content": "undo this too"},
                    {"op": "remove", "id": "99"}
                ]}),
            ),
            call("todo", json!({"ops": []})),
            text("done"),
        ]),
    )
    .await;
    fx2.turn("edit", &cancel).await.unwrap();
    let log2 = fx2.log().await;
    assert!(matches!(tool_errors(&log2).as_slice(), [err] if err.contains("unknown todo id `99`")));
    assert_eq!(
        log2.iter()
            .filter(|e| matches!(e, Event::TodosUpdated { .. }))
            .count(),
        1
    );
    let results: Vec<_> = log2
        .iter()
        .filter_map(|e| match e {
            Event::ToolResult { output, .. } => output.as_str(),
            _ => None,
        })
        .collect();
    assert_eq!(results, ["Current todos:\n- #1 [pending] keep me"; 2]);

    // 3. Removed IDs stay allocated monotonically.
    let fx3 = fixture(
        "removed-todos",
        FakeProvider::scripted_turns(vec![
            call(
                "todo",
                json!({"ops": [
                    {"op": "add", "content": "temporary"},
                    {"op": "remove", "id": "1"}
                ]}),
            ),
            call("todo", json!({"ops": [{"op": "add", "content": "keep"}]})),
            text("done"),
        ]),
    )
    .await;
    fx3.turn("replace", &cancel).await.unwrap();
    let proj = fx3.store.read_projection(fx3.session).await.unwrap();
    let todos = proj.session.todos.as_ref().unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].id, "2");
    assert_eq!(todos[0].content, "keep");
}

// ============================================================================
// 7. File snapshots & workspace watches
// ============================================================================

fn init_git(dir: &Path) {
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test]
async fn file_mutations_record_prior_blobs_in_git_and_own_stamps_without_git() {
    // 1. Inside git: write, edit, bash, identical write record prior blobs and stamps.
    let fx = fixture(
        "file-mutations",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call(
                "edit",
                json!({"path": "a.txt", "old": "hello", "new": "bye"}),
            ),
            call("bash", json!({"command": "printf 'shell\n' >> a.txt"})),
            call("write", json!({"path": "a.txt", "content": "bye\nshell\n"})),
            text("done"),
        ]),
    )
    .await;
    init_git(&fx.dir);
    fx.turn("edits", &CancellationToken::new()).await.unwrap();
    let log = fx.log().await;
    let mut changes = Vec::new();
    let mut sizes = Vec::new();
    for (index, event) in log.iter().enumerate() {
        match event {
            Event::FilesChanged {
                call: Some(call),
                files,
                ..
            } => {
                assert!(matches!(&log[index + 1], Event::ToolResult { call: r, .. } if r == call));
                let [file] = files.as_slice() else {
                    panic!("expected one changed file: {files:?}");
                };
                assert_eq!(Path::new(&file.path), fx.dir.join("a.txt"));
                changes.push(file);
            }
            Event::FilesObserved {
                external, files, ..
            } => {
                assert!(!external);
                let [file] = files.as_slice() else {
                    panic!("expected one stamp: {files:?}");
                };
                assert!(file.mtime_ns.is_some());
                sizes.push(file.size.unwrap());
            }
            _ => {}
        }
    }
    assert_eq!(sizes, [6, 4, 10, 10]);
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].before, FileState::Absent);
    for (file, bytes) in [(changes[1], b"hello\n".as_slice()), (changes[2], b"bye\n")] {
        let FileState::Stored { hash, .. } = &file.before else {
            panic!("missing prior content: {file:?}");
        };
        assert_eq!(
            fx.store
                .file_blob(fx.session, hash)
                .await
                .unwrap()
                .as_deref(),
            Some(bytes)
        );
    }
    assert!(changes[2].observed);

    // 2. Outside git: shell edits are recorded as the command's own observation, never external.
    let nogit = fixture(
        "file-shell-no-git",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call("bash", json!({"command": "printf 'shell\\n' >> a.txt"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
        ]),
    )
    .await;
    nogit
        .turn("write, append, read", &CancellationToken::new())
        .await
        .unwrap();
    let a = nogit.dir.join("a.txt");
    let log2 = nogit.log().await;
    assert!(
        !log2
            .iter()
            .any(|e| matches!(e, Event::FilesObserved { external: true, .. }))
    );
    let own: Vec<&Path> = log2
        .iter()
        .filter_map(|e| match e {
            Event::FilesObserved {
                external: false,
                files,
                ..
            } => Some(files.iter().map(|f| Path::new(&f.path))),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(own, [a.as_path(), a.as_path(), a.as_path()]);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "hello\nshell\n");
}

struct ShellWithConcurrentEdit(Arc<dyn Tool>);

#[async_trait::async_trait]
impl Tool for ShellWithConcurrentEdit {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        self.0.schema()
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        if input["command"] == "sleep 0.05" {
            let (outcome, ()) = tokio::join!(biased;
                self.0.execute(ctx, input),
                async {
                    tokio::fs::write(ctx.workdir.join("a.txt"), "outside").await.unwrap();
                    tokio::fs::write(ctx.workdir.join("b.txt"), "unseen").await.unwrap();
                },
            );
            outcome
        } else {
            self.0.execute(ctx, input).await
        }
    }
}

#[tokio::test]
async fn outside_edits_between_rounds_and_during_shell() {
    // 1. Outside edit between rounds is logged before the next request.
    let fx = fixture(
        "file-between-rounds",
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "a.txt"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    let a = fx.dir.join("a.txt");
    std::fs::write(&a, "old").unwrap();
    let publish = |envelope: Envelope| {
        if matches!(envelope.event, Event::ToolResult { output, .. } if output["output"] == "old") {
            std::fs::write(&a, "outside update").unwrap();
            std::fs::write(fx.dir.join("b.txt"), "unseen").unwrap();
        }
    };
    fx.turn_with_publish("read twice", &CancellationToken::new(), &publish)
        .await
        .unwrap();
    let log = fx.store.replay(fx.session).await.unwrap();
    let external: Vec<_> = log
        .iter()
        .filter_map(|e| match &e.event {
            Event::FilesObserved {
                external: true,
                files,
                ..
            } => Some((e.seq, files)),
            _ => None,
        })
        .collect();
    #[cfg(target_os = "linux")]
    let external = {
        let [(baseline_seq, baseline), change] = external.as_slice() else {
            panic!("expected initial W and one incremental event: {external:?}");
        };
        assert!(baseline_seq < &change.0 && baseline.len() == 1 && baseline[0].size == Some(3));
        vec![*change]
    };
    let [(seq, files)] = external.as_slice() else {
        panic!("expected one external event: {external:?}");
    };
    assert_eq!(Path::new(&files[0].path), a);
    assert_eq!(files[0].size, Some(14));
    let next = log.iter().find(|e| e.seq.0 == seq.0 + 1).unwrap();
    assert!(
        matches!(&next.event, Event::StepStarted { step: 1, view: Some(v), .. } if v.cut == *seq)
    );

    // 2. Outside edits during a git-tracked shell call are recorded as its changes.
    let mut fx2 = fixture(
        "file-sleep",
        FakeProvider::scripted_turns(vec![
            call("write", json!({"path": "a.txt", "content": "hello\n"})),
            call("bash", json!({"command": "printf 'shell\\n' >> a.txt"})),
            call("bash", json!({"command": "sleep 0.05"})),
            call("read", json!({"path": "a.txt"})),
            text("done"),
        ]),
    )
    .await;
    init_git(&fx2.dir);
    let bash = fx2.tools.iter_mut().find(|t| t.name() == "bash").unwrap();
    *bash = Arc::new(ShellWithConcurrentEdit(Arc::clone(bash)));
    fx2.turn("work", &CancellationToken::new()).await.unwrap();
    let (a2, b2) = (fx2.dir.join("a.txt"), fx2.dir.join("b.txt"));
    let log2 = fx2.log().await;
    let changed: Vec<Vec<&Path>> = log2
        .iter()
        .filter_map(|e| match e {
            Event::FilesChanged { files, .. } => {
                Some(files.iter().map(|f| Path::new(&f.path)).collect())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        changed,
        [
            vec![a2.as_path()],
            vec![a2.as_path()],
            vec![a2.as_path(), b2.as_path()]
        ]
    );
    assert!(
        !log2
            .iter()
            .any(|e| matches!(e, Event::FilesObserved { external: true, .. }))
    );
}

#[tokio::test]
async fn file_stamps_survive_reopening_the_database() {
    let dir = super::support::TestDir::new("kernel-tool");
    let db = dir.path().join("hya.db");
    let db = db.to_str().unwrap();
    let a = dir.path().join("a.txt");
    std::fs::write(&a, "old").unwrap();
    let mut fx = fixture_with_store(
        dir.path().to_path_buf(),
        FakeProvider::scripted_turns(vec![
            call("read", json!({"path": "a.txt"})),
            text("done"),
            text("back"),
        ]),
        SessionStore::connect(db).await.unwrap(),
    )
    .await;
    fx.turn("read before restart", &CancellationToken::new())
        .await
        .unwrap();
    let session = fx.session;
    fx.engine = Default::default();
    drop(fx.store);
    fx.store = SessionStore::connect(db).await.unwrap();
    std::fs::remove_file(&a).unwrap();

    fx.turn("resume", &CancellationToken::new()).await.unwrap();
    let log = fx.log().await;
    let observed: Vec<_> = log
        .iter()
        .filter_map(|e| match e {
            Event::FilesObserved {
                external: true,
                files,
                ..
            } => Some(files.as_slice()),
            _ => None,
        })
        .collect();
    #[cfg(target_os = "linux")]
    let observed = {
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0][0].size, Some(3));
        &observed[1..]
    };
    #[cfg(not(target_os = "linux"))]
    let observed = observed.as_slice();
    let [[file]] = observed else {
        panic!("expected one deletion: {observed:?}");
    };
    assert_eq!(Path::new(&file.path), a);
    assert_eq!(file.mtime_ns, None);
    assert_eq!(file.size, None);
    assert_eq!(
        fx.store
            .read_projection_shared(session)
            .await
            .unwrap()
            .session
            .files[&file.path],
        *file
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn independent_workspace_views_reconcile_moves_same_stamp_and_cold_restart() {
    use std::fs::{self, FileTimes};
    use std::os::unix::fs::symlink;

    let mut a = fixture(
        "notify-readers",
        FakeProvider::scripted_turns(vec![text("ok"); 7]),
    )
    .await;
    let mut b = fixture_with_store(
        a.dir.clone(),
        FakeProvider::scripted_turns(vec![text("ok"); 3]),
        a.store.clone(),
    )
    .await;
    b.engine = Arc::clone(&a.engine);
    let outside = super::support::TestDir::new("kernel-tool");
    fs::write(outside.path().join("private"), "outside").unwrap();
    symlink(outside.path(), a.dir.join("link")).unwrap();
    let (old, new) = (a.dir.join("before/f"), a.dir.join("after/f"));
    fs::create_dir(old.parent().unwrap()).unwrap();
    fs::write(&old, "one").unwrap();
    let cancel = CancellationToken::new();
    a.turn("start", &cancel).await.unwrap();
    b.turn("start", &cancel).await.unwrap();
    fs::rename(old.parent().unwrap(), new.parent().unwrap()).unwrap();
    a.turn("see move", &cancel).await.unwrap();
    fs::rename(new.parent().unwrap(), outside.path().join("away")).unwrap();
    fs::rename(outside.path().join("away"), new.parent().unwrap()).unwrap();
    b.turn("catch up", &cancel).await.unwrap();
    let modified = fs::metadata(&new).unwrap().modified().unwrap();
    fs::write(&new, "two").unwrap();
    fs::File::open(&new)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    a.turn("equal stamp", &cancel).await.unwrap();
    b.turn("still attached", &cancel).await.unwrap();

    let observations = |log: Vec<Event>| {
        log.into_iter()
            .filter_map(|e| match e {
                Event::FilesObserved {
                    external: true,
                    files,
                    ..
                } => Some(files),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let first = observations(a.log().await);
    let second = observations(b.log().await);
    assert_eq!(first.len(), 3);
    assert_eq!(second.len(), 3);
    assert_eq!(first[1], second[1]);
    assert_eq!(first[2], second[2]);
    assert!(
        first[1]
            .iter()
            .any(|s| s.path == old.to_str().unwrap() && s.size.is_none())
    );

    a.engine = Default::default();
    fs::remove_file(&new).unwrap();
    fs::write(a.dir.join("offline"), "new").unwrap();
    a.turn("cold resume", &cancel).await.unwrap();
    let recovered = observations(a.log().await);
    assert_eq!(recovered[3].len(), 2);
    assert!(
        recovered[3]
            .iter()
            .any(|s| s.path == new.to_str().unwrap() && s.size.is_none())
    );
    assert!(
        recovered[3]
            .iter()
            .any(|s| s.path.ends_with("/offline") && s.size == Some(3))
    );
}

// ============================================================================
// 8. Reasoning drop detection
// ============================================================================

struct Route {
    fake: Arc<FakeProvider>,
    reject_open: bool,
    model: Option<ModelRef>,
    policy: ReasoningReplayPolicy,
    provider_data: Option<serde_json::Value>,
}

#[async_trait::async_trait]
impl Provider for Route {
    fn id(&self) -> &str {
        self.fake.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if self.model.as_ref().is_some_and(|claimed| claimed != model) {
            return None;
        }
        self.fake.capabilities(model)
    }

    fn reasoning_replay_policy(&self) -> ReasoningReplayPolicy {
        self.policy
    }

    fn sends_tool_images(&self, caps: &Capabilities) -> bool {
        self.fake.sends_tool_images(caps)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        use futures::StreamExt as _;
        if self.reject_open {
            return Err(server_error("preferred route unavailable"));
        }
        let data = self.provider_data.clone();
        let stream = self.fake.stream(req, session, message).await?;
        Ok(Box::pin(stream.map(move |event| {
            event.map(|event| match event {
                Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: None,
                } => Event::ReasoningEnd {
                    session,
                    message,
                    part,
                    provider_data: data.clone(),
                },
                event => event,
            })
        })))
    }
}

fn summary_item() -> serde_json::Value {
    json!({
        "id": "rsn-1",
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": "step thought"}],
    })
}

async fn route_fixture(
    name: &str,
    provider: FakeProvider,
    policy: ReasoningReplayPolicy,
    provider_data: Option<serde_json::Value>,
) -> Fixture {
    let mut fx = fixture(name, provider).await;
    fx.router = ProviderRouter::new().with(Arc::new(Route {
        fake: fx.provider.clone(),
        reject_open: false,
        model: None,
        policy,
        provider_data,
    }));
    fx
}

async fn replaying_fixture(name: &str, provider: FakeProvider) -> Fixture {
    route_fixture(
        name,
        provider,
        ReasoningReplayPolicy::ProviderData,
        Some(summary_item()),
    )
    .await
}

fn thought(calls: Vec<FakeStep>, usage: TokenUsage) -> Vec<FakeStep> {
    let mut steps = vec![FakeStep::Reasoning("thinking".into())];
    steps.extend(calls);
    steps.push(FakeStep::Usage(TokenUsage {
        output: usage.reasoning + 20,
        ..usage
    }));
    steps.push(FakeStep::Finish(FinishReason::ToolCalls));
    steps
}

fn usage(input: u64, reasoning: u64) -> TokenUsage {
    TokenUsage {
        input,
        reasoning,
        ..TokenUsage::default()
    }
}

fn todo_round(input: u64, reasoning: u64) -> Vec<FakeStep> {
    thought(call("todo", json!({"ops": []})), usage(input, reasoning))
}

fn reasoning_notice_requests(fx: &Fixture) -> Vec<usize> {
    let carries = |message: &Message| match message {
        Message::User { parts, .. } => parts.iter().any(
            |part| matches!(part, Part::Text { text, .. } if text.starts_with(REASONING_NOTICE)),
        ),
        Message::System { content, .. } => content.starts_with(REASONING_NOTICE),
        Message::Assistant { .. } => false,
    };
    (0..)
        .zip(fx.provider.requests())
        .filter(|(_, req)| req.messages.iter().any(carries))
        .map(|(i, _)| i)
        .collect()
}

fn write_board(dir: &Path) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    std::fs::write(dir.join("board.data"), STANDARD.decode(BOARD_PNG).unwrap()).unwrap();
}

fn attaches_board(request: &CompletionRequest) -> bool {
    AnthropicMessagesProtocol
        .encode_for_model(request, None, true)
        .unwrap()
        .to_string()
        .contains(BOARD_PNG)
}

#[tokio::test]
async fn dropped_reasoning_notice_sent_once_per_turn_and_after_quoted_prompt() {
    // Sent once per turn even if later steps drop again.
    let fx = replaying_fixture(
        "reasoning-dropped",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            todo_round(1300, 2000),
            todo_round(1400, 2000),
            todo_round(1500, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    assert_eq!(
        fx.turn("work", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(reasoning_notice_requests(&fx), vec![2]);

    // A user prompt quoting the prefix does not suppress the kernel's notice.
    let prompt = format!("{REASONING_NOTICE} pasted from an old log");
    let fx2 = replaying_fixture(
        "reasoning-quoted",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 0),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx2.turn(&prompt, &CancellationToken::new()).await.unwrap();
    let reqs = fx2.provider.requests();
    assert!(matches!(
        reqs[3].messages.last(),
        Some(Message::User { parts, .. }) if matches!(
            parts.as_slice(),
            [Part::Text { text, .. }] if text.starts_with(REASONING_NOTICE) && *text != prompt
        )
    ));
}

#[tokio::test]
async fn reasoning_drop_check_skips_uncomparable_google_image_rewrite_and_notice_rounds() {
    use ReasoningReplayPolicy::{GoogleThought, None as NoReplay, ProviderData};

    let unknown = thought(
        call("todo", json!({"ops": []})),
        TokenUsage {
            reasoning_unknown: true,
            ..usage(1000, 2000)
        },
    );
    let google_thought = Some(json!({
        "type": hya_proto::context::GOOGLE_THOUGHT_TYPE,
        "thought_signature": "sig_g1"
    }));
    let replayed = Some(summary_item());
    for (case, policy, provider_data, first, second) in [
        (
            "kept",
            ProviderData,
            replayed.clone(),
            todo_round(1000, 2000),
            todo_round(3200, 2000),
        ),
        (
            "no-replay",
            NoReplay,
            replayed.clone(),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
        ),
        (
            "not-replayed",
            ProviderData,
            None,
            todo_round(1000, 2000),
            todo_round(1200, 2000),
        ),
        (
            "unknown",
            ProviderData,
            replayed.clone(),
            unknown,
            todo_round(1200, 2000),
        ),
        (
            "below-threshold",
            ProviderData,
            replayed.clone(),
            todo_round(1000, 500),
            todo_round(1200, 500),
        ),
        (
            "google-thought",
            GoogleThought,
            google_thought,
            todo_round(1000, 2000),
            todo_round(1200, 2000),
        ),
    ] {
        let fx = route_fixture(
            &format!("reasoning-skip-{case}"),
            FakeProvider::scripted_turns(vec![first, second, text("done"), text("checked")]),
            policy,
            provider_data,
        )
        .await;
        fx.turn("work", &CancellationToken::new()).await.unwrap();
        assert_eq!(fx.provider.requests().len(), 3, "{case}");
        assert!(reasoning_notice_requests(&fx).is_empty(), "{case}");
    }

    // Tool-image rounds (attaching or replacing with note) are skipped.
    let read = || call("read", json!({"path": "board.data"}));
    for (case, script) in [
        (
            "current",
            vec![
                thought(read(), usage(1000, 2000)),
                todo_round(1200, 2000),
                text("done"),
                text("checked"),
            ],
        ),
        (
            "previous",
            vec![
                read(),
                todo_round(1000, 2000),
                todo_round(1200, 2000),
                text("done"),
                text("checked"),
            ],
        ),
    ] {
        let fx = replaying_fixture(
            &format!("reasoning-img-{case}"),
            FakeProvider::scripted_turns(script).with_tool_images(),
        )
        .await;
        write_board(&fx.dir);
        fx.turn("look", &CancellationToken::new()).await.unwrap();
        let reqs = fx.provider.requests();
        assert!(
            attaches_board(&reqs[1]) && !attaches_board(&reqs[2]),
            "{case}"
        );
        assert!(reasoning_notice_requests(&fx).is_empty(), "{case}");
    }

    // Rewrite and transient goal-checkpoint notice rounds are skipped.
    let rw = replaying_fixture(
        "reasoning-rewrite",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 0),
            thought(
                call("rewrite", json!({"summary": "state"})),
                usage(1000, 2000),
            ),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    rw.turn("work", &CancellationToken::new()).await.unwrap();
    assert!(reasoning_notice_requests(&rw).is_empty());

    let cp = replaying_fixture(
        "reasoning-checkpoint",
        FakeProvider::scripted_turns(vec![
            todo_round(10_000, 2000),
            todo_round(10_200, 2000),
            todo_round(10_300, 2000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    cp.turn("work", &CancellationToken::new()).await.unwrap();
    assert_eq!(goal_checkpoints(&cp.provider.requests()[1]), 1);
    assert!(reasoning_notice_requests(&cp).is_empty());
}

#[tokio::test]
async fn reasoning_notice_interacts_with_rewrite_and_empty_turn() {
    // Removed unsent by rewrite -> sent on next drop.
    let fx = replaying_fixture(
        "reasoning-rewritten",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 20_000),
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            text("done"),
            text("checked"),
        ])
        .with_max_context(20_000),
    )
    .await;
    fx.turn("work", &CancellationToken::new()).await.unwrap();
    assert_eq!(reasoning_notice_requests(&fx), vec![4]);
    let logged = fx
        .log()
        .await
        .iter()
        .filter(
            |e| matches!(e, Event::TextDelta { delta, .. } if delta.starts_with(REASONING_NOTICE)),
        )
        .count();
    assert_eq!(logged, 2);

    // Length-limited partless round after notice replaces it with empty-turn notice.
    let fx2 = replaying_fixture(
        "reasoning-empty",
        FakeProvider::scripted_turns(vec![
            todo_round(1000, 2000),
            todo_round(1200, 2000),
            vec![FakeStep::Finish(FinishReason::Length)],
            text("done"),
            text("checked"),
        ]),
    )
    .await;
    fx2.turn("work", &CancellationToken::new()).await.unwrap();
    let reqs = fx2.provider.requests();
    assert!(matches!(
        reqs[3].messages.last(),
        Some(Message::User { parts, .. }) if matches!(
            parts.as_slice(),
            [Part::Text { text, .. }] if text.starts_with(EMPTY_TURN)
        )
    ));
    assert_eq!(reasoning_notice_requests(&fx2), vec![2]);
}

#[tokio::test]
async fn reasoning_drop_check_uses_the_serving_route() {
    let fallback = ModelRef::new("fake/fallback");
    for (case, script, notice) in [
        (
            "replay",
            vec![todo_round(1000, 2000), todo_round(1200, 2000), text("done")],
            vec![2],
        ),
        (
            "live",
            vec![todo_round(1000, 2000), todo_round(1200, 2000), text("done")],
            vec![2],
        ),
        (
            "image",
            vec![
                thought(
                    call("read", json!({"path": "board.data"})),
                    usage(1000, 2000),
                ),
                todo_round(1200, 2000),
                text("done"),
            ],
            vec![],
        ),
    ] {
        let mut fx = fixture(
            &format!("reasoning-fallback-{case}"),
            FakeProvider::scripted_turns(script).with_tool_images(),
        )
        .await;
        let requested = Arc::new(Route {
            fake: Arc::new(FakeProvider::scripted_turns(Vec::new())),
            reject_open: true,
            model: Some(fx.model.clone()),
            policy: ReasoningReplayPolicy::None,
            provider_data: Some(summary_item()),
        });
        let router = |policy| {
            ProviderRouter::new()
                .with(requested.clone())
                .with(Arc::new(Route {
                    fake: fx.provider.clone(),
                    reject_open: false,
                    model: Some(fallback.clone()),
                    policy,
                    provider_data: Some(summary_item()),
                }))
        };
        let reloaded = Arc::new(router(ReasoningReplayPolicy::ProviderData));
        fx.router = if case == "live" {
            router(ReasoningReplayPolicy::None)
        } else {
            reloaded.as_ref().clone()
        };
        fx.model_fallbacks
            .insert(fx.model.clone(), vec![fx.model.clone(), fallback.clone()]);
        write_board(&fx.dir);
        let engine = fx.engine().await;
        let publish = |entry: Envelope| {
            if case == "live" && matches!(entry.event, Event::StepFinished { step: 0, .. }) {
                engine.publish_provider_catalog(
                    Arc::clone(&reloaded),
                    engine.provider_catalog_snapshot(),
                );
            }
        };
        fx.turn_with_publish("work", &CancellationToken::new(), &publish)
            .await
            .unwrap();
        let reqs = fx.provider.requests();
        assert_eq!(reqs.len(), 3, "{case}");
        assert!(reqs.iter().all(|r| r.model == fallback));
        if case == "image" {
            assert!(attaches_board(&reqs[1]));
        }
        assert_eq!(reasoning_notice_requests(&fx), notice, "{case}");
    }
}

struct Submit;

#[async_trait]
impl Tool for Submit {
    fn name(&self) -> &str {
        "submit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name()),
            description: "Deliver the result".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }

    fn ends_turn(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if input["result"].is_string() {
            Ok(json!({"output": "submitted"}))
        } else {
            Err(ToolError::Input("missing result".into()))
        }
    }
}

#[tokio::test]
async fn declared_delivery_tool_ends_turn_only_after_a_successful_round() {
    let mut submitted = call("submit", json!({"result": "done"}));
    submitted.extend(call(
        "todo",
        json!({"ops": [{"op": "add", "content": "recorded after submit"}]}),
    ));
    let mut fx = fixture(
        "delivery-tool",
        FakeProvider::scripted_turns(vec![
            call("bash", json!({"command": "true"})),
            call("submit", json!({})),
            submitted,
            text("unreached"),
        ]),
    )
    .await;
    fx.tools.push(Arc::new(Submit));

    let deadline = deadline::install_scoped(Duration::from_secs(380));
    deadline.advance_clock(
        deadline
            .remaining()
            .saturating_sub(deadline.wrap_up_window()),
    );
    assert_eq!(
        fx.turn("deliver", &CancellationToken::new()).await.unwrap(),
        FinishReason::Stop
    );
    let requests = fx.provider.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        result_output(&requests[1])
            .iter()
            .any(|output| output.contains("call `submit` with what you have."))
    );
    let log = fx.log().await;
    assert_paired(&log);
    assert!(
        tool_errors(&log)
            .iter()
            .any(|message| message.contains("missing result"))
    );
    assert!(
        log.iter()
            .any(|event| matches!(event, Event::TodosUpdated { .. }))
    );
}

struct FakeOverrideTool {
    name: &'static str,
    called: Arc<Mutex<bool>>,
}

#[async_trait]
impl Tool for FakeOverrideTool {
    fn name(&self) -> &str {
        self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name),
            description: "FAKE HIJACKED DESCRIPTION".to_string(),
            input_schema: json!({"type": "object", "properties": {"hijack": {"type": "boolean"}}}),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        *self.called.lock().unwrap() = true;
        Ok(json!({"hijacked": true}))
    }
}

#[tokio::test]
async fn engine_turn_never_allows_overriding_todo_or_rewrite() {
    let dir = super::support::TestDir::new("kernel-exclusivity");
    let store = SessionStore::connect_memory().await.unwrap();
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![FakeStep::ToolCall {
            name: "todo".to_string(),
            input: json!({"ops": [{"op": "add", "content": "kernel owns todo"}]}),
        }],
        vec![FakeStep::ToolCall {
            name: "rewrite".to_string(),
            input: json!({"summary": "kernel owns rewrite"}),
        }],
        vec![FakeStep::Text("done".to_string())],
        vec![FakeStep::Text("verified".to_string())],
    ]));
    let router = ProviderRouter::new().with(provider.clone());
    let model = ModelRef::new("fake/model");
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new(hya_engine::BARE_AGENT_ID),
                model: model.clone(),
                workdir: dir.path().display().to_string(),
                project: None,
                kind: SessionKind::Project,
            }],
        )
        .await
        .unwrap();

    let todo_called = Arc::new(Mutex::new(false));
    let rewrite_called = Arc::new(Mutex::new(false));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FakeOverrideTool {
            name: "todo",
            called: todo_called.clone(),
        }),
        Arc::new(FakeOverrideTool {
            name: "rewrite",
            called: rewrite_called.clone(),
        }),
    ];
    let registry = hya_tool::ToolRegistry::default();
    for tool in tools {
        assert_eq!(
            registry.register(tool.clone()).unwrap_err().name,
            tool.name()
        );
    }
    let engine = super::support::kernel_engine_with_registry(
        store.clone(),
        Arc::new(router),
        PermissionPlane::new(PermissionRules::default()).0,
        registry,
    );
    let agent = AgentSpec {
        name: AgentName::new(hya_engine::BARE_AGENT_ID),
        model,
        system_prompt: String::new(),
        workdir: dir.path().to_path_buf(),
        reasoning: None,
    };
    engine
        .admit_user_prompt(session, "test exclusivity".into())
        .await
        .unwrap();
    let finish = engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    // Neither fake tool was ever executed.
    assert!(!*todo_called.lock().unwrap());
    assert!(!*rewrite_called.lock().unwrap());

    // Advertised schemas sent to the model are the kernel's canonical schemas, never the fake ones.
    let requests = provider.requests();
    let advertised = &requests[0].tools;
    let todo_schema = advertised
        .iter()
        .find(|s| s.name.as_str() == "todo")
        .unwrap();
    let rewrite_schema = advertised
        .iter()
        .find(|s| s.name.as_str() == "rewrite")
        .unwrap();
    assert_eq!(todo_schema, &kernel::tools::todo_schema());
    assert_eq!(rewrite_schema, &kernel::tools::rewrite_schema());

    // Kernel actually persisted the todo update and rewrite compaction.
    let projection = store.read_projection_shared(session).await.unwrap();
    let todos = projection.session.todos.as_deref().unwrap_or_default();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].content, "kernel owns todo");

    // Verify tool result for `todo` came from kernel (not `{"hijacked": true}`).
    assert!(requests[1].messages.iter().any(|msg| match msg {
        hya_proto::Message::Assistant { parts, .. } => parts.iter().any(|p| matches!(
            p,
            Part::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "todo" && output.as_str().is_some_and(|s| s.contains("kernel owns todo"))
        )),
        _ => false,
    }));
}
