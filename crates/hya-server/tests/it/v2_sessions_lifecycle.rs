//! Session lifecycle over `/v1`: archive/unarchive, `RefreshSessionContext`,
//! ephemeral session reaper & startup sweep, background auto-titling,
//! `RevertSession`/`ForkSession`, and restart interruption recovery.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "support/restart_fixture.rs"]
mod restart_fixture;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::stream;
use hya_api::v2 as pb;
use hya_engine::{
    BoundSidecarFactory, CoreError, CreateSession, SessionEngine, SidecarEnvironment,
    SidecarHandle, SidecarStart, TurnBinding,
};
use hya_proto::{
    AgentName, Event, FinishCause, FinishReason, Message, MessageId, ModelRef, Part, Role,
    SessionId, TokenUsage, UsagePurpose,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
};
use hya_server::{AppState, EphemeralGrace, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionRules, Rule};
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;

use crate::harness::{
    GatedProvider, TextProvider, assert_err, collect_sse, create_session, create_session_id,
    create_session_with, default_caps, engine_on, get_json, grpc_channel, id_str, ok_get, ok_patch,
    ok_post, patch, post, run_prompt, start_turn, state_of, state_with_provider, state_with_rules,
    text_events, wait_not_busy, wait_turn,
};
use crate::support;

// ---- Archive ----

#[tokio::test]
async fn session_archive_filters_inflight_unarchive_and_grpc_parity() {
    let gate = Arc::new(Semaphore::new(0));
    let state = state_with_provider(GatedProvider::new(Arc::clone(&gate))).await;
    let app = router(state.clone());
    let dir = std::env::temp_dir();

    let root = create_session(&app, &dir).await;
    let other = create_session(&app, &dir).await;
    let child = create_session_id(
        &app,
        json!({"parent": root, "workdir": dir.to_string_lossy()}),
    )
    .await;
    let root_uri = format!("/v2/sessions/{root}");

    // Archive sets flag & stamp, preserves stamp on repeat, refuses children, filters listings.
    let body = ok_patch(&app, &root_uri, json!({"archived": true})).await;
    let stamp = body["archivedAt"].clone();
    assert!(body["archived"] == json!(true) && stamp.is_string());
    assert_eq!(
        ok_patch(&app, &root_uri, json!({"archived": true})).await["archivedAt"],
        stamp
    );
    assert_err(
        patch(
            &app,
            &format!("/v2/sessions/{child}"),
            json!({"archived": true}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );

    let list_ids = |q: &'static str| {
        let app = app.clone();
        async move {
            ok_get(&app, &format!("/v2/sessions{q}")).await["sessions"]
                .as_array()
                .map_or_else(Vec::new, |r| r.iter().map(|x| id_str(&x["id"])).collect())
        }
    };
    assert!(!list_ids("").await.contains(&root));
    assert!(list_ids("?includeArchived=true").await.contains(&root));
    assert_eq!(list_ids("?archivedOnly=true").await, vec![root.clone()]);

    // gRPC UpdateSession & ListSessions archive parity.
    let mut grpc = pb::session_client::SessionClient::new(grpc_channel(state).await);
    let grpc_arch = grpc
        .list_sessions(pb::ListSessionsRequest {
            archived_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        grpc_arch
            .sessions
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec![root.as_str()]
    );
    assert!(
        !grpc
            .update_session(pb::UpdateSessionRequest {
                session: root.clone(),
                archived: Some(false),
                ..Default::default()
            })
            .await
            .unwrap()
            .into_inner()
            .archived
    );
    assert!(list_ids("").await.contains(&root) && list_ids("").await.contains(&other));

    // Archiving an inflight session lets its turn finish.
    let turn = start_turn(&app, &root, json!({"prompt": {"text": "work"}})).await;
    let patched = ok_patch(&app, &root_uri, json!({"archived": true})).await;
    assert_eq!(
        (&patched["archived"], &patched["busy"]),
        (&json!(true), &json!(true))
    );
    gate.add_permits(1);
    assert_eq!(
        wait_turn(&app, &root, &turn).await["finish"],
        json!("FINISH_REASON_STOP")
    );
    wait_not_busy(&app, &root).await;
    assert_eq!(ok_get(&app, &root_uri).await["archived"], json!(true));

    // A new prompt streams implicit unarchive; a shell turn unarchives too.
    let announced = collect_sse(&app, &format!("/v2/sessions/{root}/events/stream"), |f| {
        f["event"]["sessionUpdated"]["archived"] == json!(false)
    })
    .await;
    gate.add_permits(1);
    let turn2 = start_turn(&app, &root, json!({"prompt": {"text": "again"}})).await;
    assert!(ok_get(&app, &root_uri).await.get("archived").is_none());
    assert!(
        announced
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["sessionUpdated"]["archived"] == json!(false))
    );
    wait_turn(&app, &root, &turn2).await;
    wait_not_busy(&app, &root).await;

    let _ = ok_patch(&app, &root_uri, json!({"archived": true})).await;
    let _ = ok_post(
        &app,
        &format!("/v2/sessions/{root}/turns"),
        json!({"shell": {"command": "true"}}),
    )
    .await;
    assert!(ok_get(&app, &root_uri).await.get("archived").is_none());
}

// ---- RefreshSessionContext ----

struct RecordingSystemProvider(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl Provider for RecordingSystemProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.0
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
        Ok(Box::pin(stream::iter(
            text_events(s, m, "done").into_iter().map(Ok),
        )))
    }
}

#[tokio::test]
async fn refresh_context_invalidates_agents_md_and_checks_unknown_session() {
    let dir = support::tempdir("refresh-context");
    std::fs::write(dir.join("AGENTS.md"), "RULE_ONE_MARKER\n").unwrap();
    let systems = Arc::new(Mutex::new(Vec::new()));
    let state = state_with_provider(RecordingSystemProvider(Arc::clone(&systems))).await;
    let engine = state.engine.clone();
    let app = router(state);

    assert_eq!(
        post(
            &app,
            &format!("/v2/sessions/{}/refresh-context", SessionId::new()),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let session = create_session(&app, &dir).await;
    let refresh_uri = format!("/v2/sessions/{session}/refresh-context");
    assert_eq!(
        ok_post(&app, &refresh_uri, json!({})).await["pending"],
        Value::Null
    );

    run_prompt(&app, &session, "hi").await;
    std::fs::write(dir.join("AGENTS.md"), "RULE_TWO_MARKER\n").unwrap();
    run_prompt(&app, &session, "hi").await;
    assert_eq!(
        ok_post(&app, &refresh_uri, json!({})).await["pending"],
        true
    );
    run_prompt(&app, &session, "hi").await;

    let recorded = systems.lock().unwrap().clone();
    assert_eq!(recorded.len(), 3);
    assert!(recorded[0].contains("RULE_ONE_MARKER") && recorded[1].contains("RULE_ONE_MARKER"));
    assert!(recorded[2].contains("RULE_TWO_MARKER") && !recorded[2].contains("RULE_ONE_MARKER"));
    let kinds: Vec<_> = engine
        .replay(session.parse().unwrap())
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            Event::InitialContextRecorded { .. } => Some("recorded"),
            Event::InitialContextInvalidated { .. } => Some("invalidated"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["recorded", "invalidated", "recorded"]);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Ephemeral Sessions ----

fn ephemeral_state(store: SessionStore, grace: EphemeralGrace) -> AppState {
    let (engine, _) = engine_on(store, TextProvider, PermissionRules::default());
    state_of(engine, std::env::temp_dir()).with_ephemeral_grace(grace)
}

async fn watch_session(app: &axum::Router, session: &str) -> Body {
    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/v2/sessions/{session}/events/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.into_body()
}

async fn session_exists(app: &axum::Router, session: &str) -> bool {
    get_json(app, &format!("/v2/sessions/{session}")).await.0 == StatusCode::OK
}

async fn gone_within(app: &axum::Router, session: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if !session_exists(app, session).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

#[tokio::test]
async fn ephemeral_watched_reconnected_unclaimed_and_scratch_preservation() {
    let scratch = support::tempdir("ephemeral-scratch");
    let grace = EphemeralGrace {
        unwatched: Duration::from_millis(80),
        unclaimed: Duration::from_millis(80),
        startup: Duration::from_secs(60),
    };
    let app = router(
        ephemeral_state(SessionStore::connect_memory().await.unwrap(), grace)
            .with_scratch_root(&scratch),
    );

    // Watched session kept across multiple viewers and reconnect within grace, then reaped with notices.
    let session = create_session_id(
        &app,
        json!({"workdir": std::env::temp_dir().to_string_lossy(), "ephemeral": true}),
    )
    .await;
    let (first, second) = (
        watch_session(&app, &session).await,
        watch_session(&app, &session).await,
    );
    let (saw_del, saw_proj) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (d, p, target) = (saw_del.clone(), saw_proj.clone(), session.clone());
    let global = collect_sse(&app, "/v2/events/stream", move |f| {
        if f["event"]["sessionDeleted"].is_object() && f["event"]["session"] == target {
            d.store(true, Ordering::SeqCst);
        }
        if f["event"]["projectsUpdated"].is_object() {
            p.store(true, Ordering::SeqCst);
        }
        d.load(Ordering::SeqCst) && p.load(Ordering::SeqCst)
    })
    .await;

    drop(first);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(session_exists(&app, &session).await, "still watched");
    drop(second);
    tokio::time::sleep(Duration::from_millis(15)).await;
    let reconnected = watch_session(&app, &session).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(session_exists(&app, &session).await, "reconnected in grace");
    drop(reconnected);
    assert!(gone_within(&app, &session, Duration::from_secs(2)).await);
    let _ = global.await.unwrap();
    assert!(saw_del.load(Ordering::SeqCst) && saw_proj.load(Ordering::SeqCst));

    // Unclaimed temporary ephemeral session is reaped, leaving its scratch dir intact.
    let body = create_session_with(
        &app,
        json!({"kind": "SESSION_KIND_TEMPORARY", "ephemeral": true}),
    )
    .await
    .1;
    let temp_id = id_str(&body["session"]["id"]);
    let workdir = PathBuf::from(body["session"]["workdir"].as_str().unwrap());
    std::fs::write(workdir.join("notes.txt"), "kept").unwrap();
    assert!(gone_within(&app, &temp_id, Duration::from_secs(2)).await);
    assert_eq!(
        std::fs::read_to_string(workdir.join("notes.txt")).unwrap(),
        "kept"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[tokio::test]
async fn ephemeral_used_non_ephemeral_and_child_sessions_are_never_reaped() {
    let grace = EphemeralGrace {
        unwatched: Duration::from_millis(40),
        unclaimed: Duration::from_secs(60),
        startup: Duration::from_secs(60),
    };
    let app = router(ephemeral_state(
        SessionStore::connect_memory().await.unwrap(),
        grace,
    ));
    let wd = std::env::temp_dir().to_string_lossy().into_owned();

    let prompted = create_session_id(&app, json!({"workdir": wd, "ephemeral": true})).await;
    let titled = create_session_id(&app, json!({"workdir": wd, "ephemeral": true})).await;
    let archived = create_session_id(&app, json!({"workdir": wd, "ephemeral": true})).await;
    let forked = create_session_id(&app, json!({"workdir": wd, "ephemeral": true})).await;
    let watchers = [
        watch_session(&app, &prompted).await,
        watch_session(&app, &titled).await,
        watch_session(&app, &archived).await,
        watch_session(&app, &forked).await,
    ];

    run_prompt(&app, &prompted, "hi").await;
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{titled}"),
        json!({"title": "Named"}),
    )
    .await;
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{archived}"),
        json!({"archived": true}),
    )
    .await;
    assert_ne!(
        ok_post(&app, &format!("/v2/sessions/{forked}/fork"), json!({})).await["session"]["ephemeral"],
        true
    );

    let plain = create_session_id(&app, json!({"workdir": wd})).await;
    let child_obj = create_session_with(
        &app,
        json!({"workdir": wd, "parent": plain, "ephemeral": true}),
    )
    .await
    .1["session"]
        .clone();
    assert_ne!(child_obj["ephemeral"], true);
    let child = id_str(&child_obj["id"]);
    let pre_obj = create_session_with(
        &app,
        json!({"workdir": wd, "title": "Pre", "ephemeral": true}),
    )
    .await
    .1["session"]
        .clone();
    assert_ne!(pre_obj["ephemeral"], true);
    let pretitled = id_str(&pre_obj["id"]);

    drop(watchers);
    for s in [&plain, &child, &pretitled] {
        drop(watch_session(&app, s).await);
    }
    tokio::time::sleep(Duration::from_millis(80)).await;
    for s in [
        &prompted, &titled, &archived, &forked, &plain, &child, &pretitled,
    ] {
        assert!(session_exists(&app, s).await, "{s} should stay");
    }
}

#[tokio::test]
async fn ephemeral_startup_sweep_drops_abandoned_and_spares_used_plain_and_rewatched() {
    let dir = support::tempdir("ephemeral-sweep");
    let db = dir.join("sessions.db").to_string_lossy().into_owned();
    let (abandoned, rewatched, used, plain) = {
        let (engine, _) = engine_on(
            SessionStore::connect(&db).await.unwrap(),
            TextProvider,
            PermissionRules::default(),
        );
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(
                engine
                    .create(CreateSession {
                        parent: None,
                        agent: AgentName::new("build"),
                        model: ModelRef::new("fake"),
                        workdir: std::env::temp_dir().to_string_lossy().into_owned(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    })
                    .await
                    .unwrap(),
            );
        }
        for &id in ids.iter().take(3) {
            engine.set_session_ephemeral(id, true).await.unwrap();
        }
        engine.set_title(ids[2], "Used".into()).await.unwrap();
        (ids[0], ids[1], ids[2], ids[3])
    };
    let app = router(ephemeral_state(
        SessionStore::connect(&db).await.unwrap(),
        EphemeralGrace {
            unwatched: Duration::from_secs(60),
            unclaimed: Duration::from_secs(60),
            startup: Duration::from_millis(45),
        },
    ));
    let viewer = watch_session(&app, &rewatched.to_string()).await;
    assert!(gone_within(&app, &abandoned.to_string(), Duration::from_secs(2)).await);
    tokio::time::sleep(Duration::from_millis(60)).await;
    for s in [rewatched, used, plain] {
        assert!(session_exists(&app, &s.to_string()).await);
    }
    drop(viewer);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Automatic Titles ----

struct TitlingProvider {
    title: Option<String>,
    calls: Arc<Mutex<Vec<String>>>,
    called: Arc<Notify>,
}

#[async_trait]
impl Provider for TitlingProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then(default_caps)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let script = if req
            .system
            .as_deref()
            .is_some_and(|s| s.contains("You are a title generator."))
        {
            let prompt = req
                .messages
                .iter()
                .filter_map(|m| match m {
                    Message::User { parts, .. } => Some(
                        parts
                            .iter()
                            .filter_map(|p| match p {
                                Part::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.calls.lock().unwrap().push(prompt);
            self.called.notify_waiters();
            let Some(title) = &self.title else {
                return Err(ProviderError::Incompatible("title model down".into()));
            };
            vec![
                FakeStep::Text(format!("{title}\n")),
                FakeStep::Usage(TokenUsage {
                    input: 40,
                    output: 6,
                    ..Default::default()
                }),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            vec![
                FakeStep::Text("ok".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        };
        Ok(Box::pin(stream::iter(
            FakeProvider::materialize(&script, session, message)
                .into_iter()
                .map(Ok),
        )))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_title_runs_once_for_first_root_prompt_and_handles_skip_and_failure() {
    let store = SessionStore::connect_memory().await.unwrap();
    let dir = support::tempdir("auto-title");
    let build_app = |store: SessionStore, title: Option<&str>| {
        let (calls, called) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Notify::new()));
        let (engine, _) = engine_on(
            store,
            TitlingProvider {
                title: title.map(str::to_owned),
                calls: Arc::clone(&calls),
                called: Arc::clone(&called),
            },
            PermissionRules::default(),
        );
        (
            router(state_of(Arc::clone(&engine), dir.clone()).with_auto_title(true)),
            engine,
            calls,
            called,
        )
    };

    let (app, engine, calls, _) = build_app(store.clone(), Some("Fix login bug"));
    let session = create_session(&app, &dir).await;
    let collector = collect_sse(
        &app,
        &format!("/v2/sessions/{session}/events/stream"),
        |f| f["event"]["sessionUpdated"]["title"].is_string(),
    )
    .await;
    run_prompt(&app, &session, "please fix the login bug").await;
    assert!(
        collector
            .await
            .unwrap()
            .iter()
            .any(|f| f["event"]["sessionUpdated"]["title"] == json!("Fix login bug"))
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{session}")).await["title"],
        json!("Fix login bug")
    );
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &["please fix the login bug"]
    );
    assert!(
        engine
            .replay(session.parse().unwrap())
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(
                &e.event,
                Event::UsageRecorded {
                    purpose: UsagePurpose::Title,
                    message: None,
                    ..
                }
            ))
    );

    // Later turn, manual title, child session, and restart never trigger another title call.
    run_prompt(&app, &session, "second turn").await;
    let manual = create_session_id(
        &app,
        json!({"workdir": dir.to_string_lossy(), "title": "Own"}),
    )
    .await;
    run_prompt(&app, &manual, "do work").await;
    let child = create_session_id(
        &app,
        json!({"workdir": dir.to_string_lossy(), "parent": session}),
    )
    .await;
    run_prompt(&app, &child, "subagent work").await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{manual}")).await["title"],
        json!("Own")
    );
    assert!(ok_get(&app, &format!("/v2/sessions/{child}")).await["title"].is_null());

    let (restarted, _, restarted_calls, _) = build_app(store.clone(), Some("Other"));
    run_prompt(&restarted, &session, "after restart").await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(restarted_calls.lock().unwrap().is_empty());

    let (failing, _, fail_calls, fail_called) =
        build_app(SessionStore::connect_memory().await.unwrap(), None);
    let s = create_session(&failing, &dir).await;
    let notified = fail_called.notified();
    run_prompt(&failing, &s, "hello").await;
    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .unwrap();
    assert_eq!(fail_calls.lock().unwrap().len(), 1);
    assert!(ok_get(&failing, &format!("/v2/sessions/{s}")).await["title"].is_null());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Revert & Fork ----

fn write_turn(content: &str) -> Vec<Vec<FakeStep>> {
    vec![
        vec![
            FakeStep::ToolCall {
                name: "write".into(),
                input: json!({"path": "a.txt", "content": content}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]
}

async fn edit_fixture(
    label: &str,
    script: Vec<Vec<FakeStep>>,
) -> (axum::Router, Arc<SessionEngine>, PathBuf) {
    let dir = support::tempdir(label).canonicalize().unwrap();
    let rules = PermissionRules::new(vec![
        Rule::new(Action::Edit, "**", Mode::Allow),
        Rule::new(Action::Read, "**", Mode::Allow),
    ]);
    let (state, _, engine) =
        state_with_rules(FakeProvider::scripted_turns(script), rules, dir.clone()).await;
    (router(state), engine, dir)
}

#[tokio::test]
async fn revert_hides_turns_restores_files_commits_and_maps_errors() {
    let (app, engine, dir) = edit_fixture(
        "v1-revert",
        [write_turn("one"), write_turn("two"), write_turn("three")].concat(),
    )
    .await;
    let a = dir.join("a.txt");
    let session = create_session(&app, &dir).await;
    let rev_uri = format!("/v2/sessions/{session}/revert");
    assert_eq!(
        post(&app, &rev_uri, json!({})).await.0,
        StatusCode::BAD_REQUEST
    );

    run_prompt(&app, &session, "first").await;
    run_prompt(&app, &session, "second").await;
    let msgs = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await["messages"]
        .as_array()
        .cloned()
        .unwrap();
    let (assistant_id, second_user) = (id_str(&msgs[1]["id"]), id_str(&msgs[2]["id"]));

    // Error mapping: assistant target -> 400, unknown msg -> 404, untilSeq -> 400, busy -> 409.
    assert_eq!(
        post(&app, &rev_uri, json!({"messageId": assistant_id}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_err(
        post(
            &app,
            &rev_uri,
            json!({"messageId": "msg_00000000-0000-7000-8000-000000000000"}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(
        post(&app, &rev_uri, json!({"untilSeq": "3"})).await.0,
        StatusCode::BAD_REQUEST
    );
    let lease = engine.try_begin_turn(session.parse().unwrap()).unwrap();
    assert_err(
        post(&app, &rev_uri, json!({})).await,
        StatusCode::CONFLICT,
        "session_busy",
    );
    drop(lease);

    // Revert hides turn & restores file, replays `sessionReverted`, undo restores both.
    let rev = ok_post(&app, &rev_uri, json!({})).await;
    assert_eq!(
        (
            &rev["session"]["revert"]["messageId"],
            &rev["session"]["revert"]["hiddenMessages"],
            &rev["files"][0]["action"]
        ),
        (&json!(second_user), &json!(2), &json!("restored"))
    );
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "one");
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{session}/messages")).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["sessionReverted"]["messageId"] == json!(second_user))
    );

    assert!(
        ok_post(&app, &rev_uri, json!({"undo": true})).await["session"]
            .get("revert")
            .is_none()
    );
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "two");
    assert_err(
        post(&app, &rev_uri, json!({"undo": true})).await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );

    // Next prompt commits a pending revert.
    let _ = ok_post(&app, &rev_uri, json!({})).await;
    run_prompt(&app, &session, "third").await;
    assert!(
        ok_get(&app, &format!("/v2/sessions/{session}"))
            .await
            .get("revert")
            .is_none()
    );
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "three");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fork_copies_head_or_cut_preserves_metadata_and_maps_errors() {
    let (app, engine, dir) =
        edit_fixture("v1-fork", [write_turn("one"), write_turn("two")].concat()).await;
    let session = create_session(&app, &dir).await;
    let fork_uri = format!("/v2/sessions/{session}/fork");
    run_prompt(&app, &session, "first").await;
    run_prompt(&app, &session, "second").await;
    let msgs = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await["messages"]
        .as_array()
        .cloned()
        .unwrap();
    let (assistant_id, second_user) = (id_str(&msgs[1]["id"]), id_str(&msgs[2]["id"]));

    // Head fork copies all 4 messages, records `forkedFrom`, and titles `<id> (fork)`.
    let head = ok_post(&app, &fork_uri, json!({})).await;
    let head_id = id_str(&head["session"]["id"]);
    assert_eq!(
        (
            &head["session"]["forkedFrom"]["session"],
            &head["session"]["title"]
        ),
        (&json!(session), &json!(format!("{session} (fork)")))
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{head_id}/messages")).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // Forking a titled session adds `(fork)` once and does not stack it.
    engine
        .set_title(session.parse().unwrap(), "Plan".into())
        .await
        .unwrap();
    let tf = ok_post(&app, &fork_uri, json!({})).await["session"].clone();
    assert_eq!(tf["title"], json!("Plan (fork)"));
    assert_eq!(
        ok_post(
            &app,
            &format!("/v2/sessions/{}/fork", tf["id"].as_str().unwrap()),
            json!({})
        )
        .await["session"]["title"],
        json!("Plan (fork)")
    );

    // Cut by messageId and untilSeq; bad targets return 400/404.
    let cut = ok_post(&app, &fork_uri, json!({"messageId": second_user})).await;
    assert_eq!(
        (
            &cut["promptText"],
            &cut["session"]["forkedFrom"]["messageId"]
        ),
        (&json!("second"), &json!(second_user))
    );
    assert_eq!(
        ok_get(
            &app,
            &format!(
                "/v2/sessions/{}/messages",
                cut["session"]["id"].as_str().unwrap()
            )
        )
        .await["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let started: u64 = ok_get(&app, &format!("/v2/sessions/{session}/events")).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["messageStarted"]["message"] == json!(second_user))
        .and_then(|e| e["seq"].as_str())
        .unwrap()
        .parse()
        .unwrap();
    let cut_seq = ok_post(
        &app,
        &fork_uri,
        json!({"untilSeq": (started - 1).to_string()}),
    )
    .await;
    assert_eq!(
        ok_get(
            &app,
            &format!(
                "/v2/sessions/{}/messages",
                cut_seq["session"]["id"].as_str().unwrap()
            )
        )
        .await["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        post(&app, &fork_uri, json!({"messageId": assistant_id}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            &app,
            &fork_uri,
            json!({"messageId": "msg_00000000-0000-7000-8000-000000000000"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // Forking a reverted session copies only visible messages; temporary kind is preserved.
    let _ = ok_post(&app, &format!("/v2/sessions/{session}/revert"), json!({})).await;
    let rev_fork = id_str(&ok_post(&app, &fork_uri, json!({})).await["session"]["id"]);
    assert_eq!(
        ok_get(&app, &format!("/v2/sessions/{rev_fork}/messages")).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let temp_src = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Temporary,
        })
        .await
        .unwrap();
    let temp_fork: SessionId = ok_post(&app, &format!("/v2/sessions/{temp_src}/fork"), json!({}))
        .await["session"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        engine
            .read_projection(temp_fork)
            .await
            .unwrap()
            .session
            .kind,
        hya_proto::SessionKind::Temporary
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Restart Interruption & Recovery ----

#[derive(Clone, Default)]
struct GatedSidecar {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl SidecarEnvironment for GatedSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::new(self.clone())))
    }
}
#[async_trait]
impl BoundSidecarFactory for GatedSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}
#[async_trait]
impl SidecarHandle for GatedSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

struct FailingSidecar;
impl SidecarEnvironment for FailingSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Err(CoreError::Invalid("sidecar preparation failed".into()))
    }
}

#[tokio::test]
async fn restart_prestart_cancel_failure_and_chained_restore() {
    use restart_fixture::*;

    // 1. Pre-start sidecar interruption + chained restore tracks pending resume.
    let (store, provider, engine, agent, session) = fixture().await;
    let sidecar = Arc::new(GatedSidecar::default());
    let state = AppState::new(
        Arc::new(
            next_engine(&store, provider.clone(), engine.runtime_owner())
                .with_sidecar_environment(sidecar.clone()),
        ),
        agent.clone(),
    );
    assert_eq!(
        post_prompt(router(state.clone()), session).await,
        StatusCode::OK
    );
    entered(&sidecar.entered).await;
    state
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    let first_row = store.list_pending_resumes().await.unwrap();
    assert_eq!(first_row.len(), 1);
    assert_eq!(
        messages(&store.read_projection(session).await.unwrap()),
        vec![
            (Role::User, Some(FinishReason::Stop), None),
            (
                Role::Assistant,
                Some(FinishReason::Cancelled),
                Some(FinishCause::Shutdown)
            ),
        ]
    );
    let sidecar2 = Arc::new(GatedSidecar::default());
    let second = AppState::new(
        Arc::new(
            next_engine(&store, provider.clone(), engine.runtime_owner())
                .with_sidecar_environment(sidecar2.clone()),
        ),
        agent.clone(),
    );
    assert_eq!(second.resume_interrupted_turns().await, vec![session]);
    entered(&sidecar2.entered).await;
    assert_eq!(restore_count(&store, session).await, 1);
    second
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    let second_row = store.list_pending_resumes().await.unwrap();
    assert!(second_row.len() == 1 && second_row[0].message != first_row[0].message);

    // 2. User cancel via HTTP before restored assistant starts clears pending resume.
    let sidecar3 = Arc::new(GatedSidecar::default());
    let third = AppState::new(
        Arc::new(
            next_engine(&store, provider.clone(), engine.runtime_owner())
                .with_sidecar_environment(sidecar3.clone()),
        ),
        agent.clone(),
    );
    let app = router(third.clone());
    assert_eq!(third.resume_interrupted_turns().await, vec![session]);
    entered(&sidecar3.entered).await;
    let _ = ok_post(
        &app,
        &format!("/v2/sessions/{session}/turns/test/cancel"),
        Value::Null,
    )
    .await;
    wait_idle(app, session).await;
    let last = store
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .pop()
        .unwrap();
    assert_eq!(
        (last.role, last.finish, last.cause),
        (
            Role::Assistant,
            Some(FinishReason::Cancelled),
            Some(FinishCause::UserCancel)
        )
    );
    assert!(
        store.list_pending_resumes().await.unwrap().is_empty()
            && provider.requests.lock().unwrap().is_empty()
    );

    // 3. Sidecar prep failure before restored assistant starts closes prompt with error.
    let (store, provider, engine, agent, session) = fixture().await;
    seed_interruption(&store, session).await;
    let state = AppState::new(
        Arc::new(
            next_engine(&store, provider.clone(), engine.runtime_owner())
                .with_sidecar_environment(Arc::new(FailingSidecar)),
        ),
        agent,
    );
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    wait_idle(router(state), session).await;
    let last = store
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .pop()
        .unwrap();
    assert_eq!(
        (last.role, last.finish, last.cause),
        (Role::Assistant, Some(FinishReason::Error), None)
    );
    assert!(
        last.error
            .unwrap()
            .message
            .contains("sidecar preparation failed")
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn undo_after_interruption_drops_pending_restore() {
    use restart_fixture::*;

    for injected in [false, true] {
        let (store, provider, engine, agent, session) = fixture().await;
        engine
            .admit_user_prompt(session, "work".into())
            .await
            .unwrap();
        seed_interruption(&store, session).await;
        if injected {
            assert!(
                engine
                    .inject_restart_prompt(session)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        let app = router(AppState::new(engine.clone(), agent.clone()));
        let _ = ok_post(&app, &format!("/v2/sessions/{session}/revert"), json!({})).await;
        let next = AppState::new(
            Arc::new(next_engine(
                &store,
                provider.clone(),
                engine.runtime_owner(),
            )),
            agent,
        );
        assert!(next.resume_interrupted_turns().await.is_empty());
        assert!(store.list_pending_resumes().await.unwrap().is_empty());
        assert!(
            store
                .read_projection(session)
                .await
                .unwrap()
                .session
                .revert
                .is_some()
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}
