//! Dual-transport conformance and single-port routing: HTTP and gRPC answer
//! identically over one `AppState`, every `List*` RPC matches across both
//! transports, single-port and extra listeners share state, and background
//! drivers start once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use http_body_util::{BodyExt, Full};
use hya_api::v1 as pb;
use hya_engine::TurnBinding;
use hya_proto::{FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep};
use hya_server::{
    AgentModelControl, AgentModelControlError, AgentModelControlFuture, AgentModelIdentity,
    AgentModelState, AppState, BundleControl, BundleControlFuture, BundleListing, ProviderChange,
    ProviderControl, ProviderControlError, ProviderControlFuture, ProviderModelOverride,
    ProviderSettings, ProviderUpsert, V1Grpc, build, router,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::harness::{
    GatedProvider, assert_bad, assert_err, create_session_id, enc, get_json, grpc_channel,
    grpc_code, grpc_json, id_str, ok_get, ok_patch, ok_post, post, put, serve_server,
    state_with_provider, wait_not_busy,
};
use crate::support;

async fn parity_state() -> AppState {
    let steps = vec![vec![
        FakeStep::Text("grpc parity hello".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]];
    state_with_provider(FakeProvider::scripted_turns(steps)).await
}

fn normalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let is_vol = matches!(
                        k.as_str(),
                        "timeCreated" | "timeUpdated" | "timeRecorded" | "id"
                    );
                    let val = if is_vol {
                        Value::String("<v>".into())
                    } else {
                        normalize(v)
                    };
                    (k.clone(), val)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        other => other.clone(),
    }
}

async fn http_conn(addr: SocketAddr) -> hyper::client::conn::http1::SendRequest<Full<Bytes>> {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (s, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(conn);
    s
}

fn http_req(
    addr: SocketAddr,
    m: &str,
    uri: &str,
    body: Option<Value>,
) -> hyper::Request<Full<Bytes>> {
    let bytes = Bytes::from(body.map(|b| b.to_string()).unwrap_or_default());
    hyper::Request::builder()
        .method(m)
        .uri(uri)
        .header("host", addr.to_string())
        .header("content-type", "application/json")
        .body(Full::new(bytes))
        .unwrap()
}

async fn tcp_ok(addr: SocketAddr, method: &str, uri: &str, body: Option<Value>) -> Value {
    let req = http_req(addr, method, uri, body);
    let resp = http_conn(addr).await.send_request(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, 200, "{method} {uri}: {val}");
    val
}

async fn wait_tcp_notice_and_quiet(
    body: &mut hyper::body::Incoming,
    pred: impl Fn(&Value) -> bool,
    quiet: Duration,
) -> usize {
    let (mut text, mut matches, mut saw) = (String::new(), 0usize, false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let wait = if saw {
            quiet
        } else {
            deadline.saturating_duration_since(tokio::time::Instant::now())
        };
        let Ok(Some(Ok(frame))) = tokio::time::timeout(wait, body.frame()).await else {
            break;
        };
        let Some(data) = frame.data_ref() else {
            continue;
        };
        text.push_str(&String::from_utf8_lossy(data));
        while let Some(end) = text.find("\n\n") {
            for line in text.drain(..end + 2).collect::<String>().lines() {
                if let Some(d) = line.strip_prefix("data:")
                    && let Ok(v) = serde_json::from_str::<Value>(d.trim())
                    && pred(&v)
                {
                    matches += 1;
                    saw = true;
                }
            }
        }
    }
    matches
}

#[tokio::test]
async fn http_and_grpc_answers_and_errors_match_across_representative_calls() {
    use pb::agent_models_client::AgentModelsClient;
    use pb::catalog_client::CatalogClient;
    use pb::events_client::EventsClient;
    use pb::process_client::ProcessClient;
    use pb::session_client::SessionClient;
    use pb::turn_client::TurnClient;
    use pb::workflow_client::WorkflowClient;

    let state = parity_state().await;
    let app = router(state.clone());
    let ch = grpc_channel(state).await;
    let mut process = ProcessClient::new(ch.clone());
    let mut catalog = CatalogClient::new(ch.clone());
    let mut session = SessionClient::new(ch.clone());
    let mut turn = TurnClient::new(ch.clone());
    let mut events = EventsClient::new(ch.clone());
    let mut workflows = WorkflowClient::new(ch.clone());
    let mut agent_models = AgentModelsClient::new(ch);

    // Malformed pagination cursors rejected identically on both transports.
    for (cursor, q) in [("!", "%21"), ("AA", "AA"), ("AAAAAAAAAAAA", "AAAAAAAAAAAA")] {
        let page = Some(pb::PageRequest {
            cursor: cursor.into(),
            limit: 1,
        });
        let req_s = pb::ListSessionsRequest {
            page: page.clone(),
            ..Default::default()
        };
        let req_w = pb::ListWorkflowsRequest {
            page,
            ..Default::default()
        };
        assert_eq!(
            grpc_code(session.list_sessions(req_s).await),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            grpc_code(workflows.list_workflows(req_w).await),
            tonic::Code::InvalidArgument
        );
        for path in ["/v1/sessions", "/v1/workflows"] {
            assert_bad(get_json(&app, &format!("{path}?page.limit=1&page.cursor={q}")).await);
        }
    }

    // Health, models, providers, and permission modes parity.
    let gh = grpc_json(process.get_health(pb::GetHealthRequest {}).await);
    assert_eq!(normalize(&gh), normalize(&ok_get(&app, "/v1/health").await));
    let gm = grpc_json(catalog.list_models(pb::ListModelsRequest::default()).await);
    assert_eq!(normalize(&gm), normalize(&ok_get(&app, "/v1/models").await));
    let gp = grpc_json(
        catalog
            .list_providers(pb::ListProvidersRequest::default())
            .await,
    );
    assert_eq!(
        normalize(&gp),
        normalize(&ok_get(&app, "/v1/providers").await)
    );
    let modes = grpc_json(
        catalog
            .list_permission_modes(pb::ListPermissionModesRequest::default())
            .await,
    );
    assert_eq!(modes, ok_get(&app, "/v1/permission-modes").await);
    assert_eq!(modes["modes"][1]["id"], json!("yolo"));

    // Session create, permission mode, turn, events, fork, revert parity.
    let wd = std::env::temp_dir().to_string_lossy().into_owned();
    let create = pb::CreateSessionRequest {
        agent: "build".into(),
        model: "fake".into(),
        workdir: Some(wd),
        ..Default::default()
    };
    let g_sess = grpc_json(session.create_session(create.clone()).await);
    let h_sess = ok_post(&app, "/v1/sessions", serde_json::to_value(&create).unwrap()).await;
    assert_eq!(
        (&g_sess["session"]["agent"], &h_sess["session"]["agent"]),
        (&json!("build"), &json!("build"))
    );
    let sid = id_str(&g_sess["session"]["id"]);
    let s_uri = format!("/v1/sessions/{sid}");

    let upd = pb::UpdateSessionRequest {
        session: sid.clone(),
        permission_mode: Some("yolo".into()),
        ..Default::default()
    };
    assert_eq!(
        grpc_json(session.update_session(upd).await)["permissionMode"],
        json!("yolo")
    );
    assert_eq!(
        ok_patch(&app, &s_uri, json!({"permissionMode": "manual"})).await["permissionMode"],
        json!("manual")
    );
    let bad_upd = pb::UpdateSessionRequest {
        session: sid.clone(),
        permission_mode: Some("danger".into()),
        ..Default::default()
    };
    assert_eq!(
        grpc_code(session.update_session(bad_upd).await),
        tonic::Code::InvalidArgument
    );

    let prompt = pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
        text: "say hello".into(),
        ..Default::default()
    });
    let created = turn
        .create_turn(pb::CreateTurnRequest {
            session: sid.clone(),
            kind: Some(prompt),
        })
        .await
        .unwrap()
        .into_inner();
    let tid = created.turn.unwrap().id;
    let wait = pb::WaitTurnRequest {
        session: sid.clone(),
        turn: tid,
        timeout_ms: 10_000,
    };
    let fin = turn.wait_turn(wait).await.unwrap().into_inner();
    assert_eq!(
        (fin.state, fin.finish),
        (
            pb::TurnState::Finished as i32,
            pb::FinishReason::Stop as i32
        )
    );

    let g_ev = grpc_json(
        events
            .list_events(pb::ListEventsRequest {
                session: sid.clone(),
                ..Default::default()
            })
            .await,
    );
    let h_ev = ok_get(&app, &format!("{s_uri}/events")).await;
    assert_eq!(
        g_ev["events"].as_array().map(Vec::len),
        h_ev["events"].as_array().map(Vec::len)
    );

    let g_fork = grpc_json(
        session
            .fork_session(pb::ForkSessionRequest {
                session: sid.clone(),
                ..Default::default()
            })
            .await,
    );
    let h_fork = ok_post(&app, &format!("{s_uri}/fork"), json!({})).await;
    assert_eq!(
        g_fork["session"]["forkedFrom"],
        h_fork["session"]["forkedFrom"]
    );
    assert_eq!(g_fork["session"]["forkedFrom"]["session"], json!(sid));

    wait_not_busy(&app, &sid).await;
    let g_rev = grpc_json(
        session
            .revert_session(pb::RevertSessionRequest {
                session: sid.clone(),
                ..Default::default()
            })
            .await,
    );
    assert!(g_rev["session"]["revert"]["messageId"].is_string());
    let rev_uri = format!("{s_uri}/revert");
    assert!(
        ok_post(&app, &rev_uri, json!({"undo": true})).await["session"]
            .get("revert")
            .is_none()
    );
    let undo_req = pb::RevertSessionRequest {
        session: sid.clone(),
        undo: true,
        ..Default::default()
    };
    assert_eq!(
        grpc_code(session.revert_session(undo_req).await),
        tonic::Code::InvalidArgument
    );
    assert_bad(post(&app, &rev_uri, json!({"undo": true})).await);

    // Unavailable controls and missing-session error parity.
    assert_eq!(
        grpc_code(
            agent_models
                .list_agent_models(pb::ListAgentModelsRequest::default())
                .await
        ),
        tonic::Code::Unavailable
    );
    assert_err(
        get_json(&app, "/v1/agent-models").await,
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
    );
    let up = pb::UpsertProviderRequest {
        provider_id: "acme".into(),
        kind: "openai".into(),
        base_url: "https://acme.example/v1".into(),
        ..Default::default()
    };
    assert_eq!(
        grpc_code(catalog.upsert_provider(up).await),
        tonic::Code::Unavailable
    );
    assert_err(
        put(
            &app,
            "/v1/providers/acme",
            json!({"kind": "openai", "baseUrl": "https://acme.example/v1"}),
        )
        .await,
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
    );

    let missing = "hysec_00000000000000000000".to_owned();
    assert_eq!(
        grpc_code(
            session
                .get_session(pb::GetSessionRequest {
                    session: missing.clone()
                })
                .await
        ),
        tonic::Code::NotFound
    );
    assert_err(
        get_json(&app, &format!("/v1/sessions/{missing}")).await,
        StatusCode::NOT_FOUND,
        "session_not_found",
    );
}

#[tokio::test]
async fn vcs_diff_honors_paths_over_http_and_grpc() {
    if !support::git_available() {
        return;
    }
    let repo = support::init_git_repo("vcs-diff-paths");
    std::fs::write(repo.join("other.md"), "other\n").unwrap();
    support::git(&repo, &["add", "other.md"]);
    support::git(&repo, &["commit", "-m", "other"]);
    for (f, c) in [
        ("README.md", "hello\nchanged\n"),
        ("other.md", "other\nchanged\n"),
        ("new.txt", "fresh\n"),
        ("skip.txt", "skip\n"),
    ] {
        std::fs::write(repo.join(f), c).unwrap();
    }
    let dir = repo.to_string_lossy().into_owned();
    let q = enc(&dir);
    let state = parity_state().await;
    let app = router(state.clone());

    let all = id_str(&ok_get(&app, &format!("/v1/vcs/diff?directory={q}")).await["diff"]);
    for file in ["README.md", "other.md", "new.txt", "skip.txt"] {
        assert!(all.contains(file));
    }

    let filtered = id_str(
        &ok_get(
            &app,
            &format!("/v1/vcs/diff?directory={q}&paths=README.md&paths=new.txt"),
        )
        .await["diff"],
    );
    assert!(
        filtered.contains("README.md")
            && filtered.contains("new.txt")
            && !filtered.contains("other.md")
            && !filtered.contains("skip.txt")
    );
    assert_bad(get_json(&app, &format!("/v1/vcs/diff?directory={q}&paths=../x")).await);

    let req = pb::GetVcsDiffRequest {
        directory: dir,
        raw: false,
        paths: vec!["README.md".into(), "new.txt".into()],
    };
    let grpc = pb::project_client::ProjectClient::new(grpc_channel(state).await)
        .get_vcs_diff(req)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(grpc.diff, filtered);
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test]
async fn subagent_control_refusals_match_over_http_and_grpc() {
    let state = parity_state().await;
    let app = router(state.clone());
    let mut session = pb::session_client::SessionClient::new(grpc_channel(state).await);
    let wd = std::env::temp_dir().to_string_lossy().into_owned();
    let root = create_session_id(&app, json!({"workdir": wd})).await;
    let child = create_session_id(&app, json!({"parent": root, "workdir": wd})).await;

    for (id, expected) in [(&root, "not a subagent"), (&child, "not supervised")] {
        let (s, body) = post(&app, &format!("/v1/sessions/{id}/archive"), json!({})).await;
        assert_eq!(
            (s, &body["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("invalid_argument"))
        );
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains(expected), "{msg}");
        let err = session
            .archive_subagent(pb::ArchiveSubagentRequest {
                session: id.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            (err.code(), err.message()),
            (tonic::Code::InvalidArgument, msg)
        );
    }
    let (s, body) = post(
        &app,
        &format!("/v1/sessions/{root}/steer"),
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a subagent")
    );
    let err = session
        .steer_subagent(pb::SteerSubagentRequest {
            session: child,
            text: "  ".into(),
        })
        .await
        .unwrap_err();
    assert!(err.code() == tonic::Code::InvalidArgument && err.message().contains("needs text"));
}

#[tokio::test]
async fn every_list_rpc_has_http_grpc_page_parity() {
    use pb::agent_models_client::AgentModelsClient;
    use pb::auth_client::AuthClient;
    use pb::bundle_api_client::BundleApiClient;
    use pb::catalog_client::CatalogClient;
    use pb::events_client::EventsClient;
    use pb::files_client::FilesClient;
    use pb::interactions_client::InteractionsClient;
    use pb::messages_client::MessagesClient;
    use pb::project_client::ProjectClient;
    use pb::pty_client::PtyClient;
    use pb::session_client::SessionClient;
    use pb::workflow_client::WorkflowClient;
    use pb::worktrees_client::WorktreesClient;

    let state = parity_state()
        .await
        .with_agent_model_control(Arc::new(EmptyListControl))
        .with_provider_control(Arc::new(EmptyListControl))
        .with_bundle_control(Arc::new(EmptyListControl));
    let app = router(state.clone());
    let ch = grpc_channel(state).await;
    let dir = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(create_session_id(&app, json!({"workdir": dir})).await);
    }
    let sess = &ids[0];
    let proj = id_str(&ok_get(&app, "/v1/projects").await["projects"][0]["id"]);
    let mut covered = std::collections::BTreeSet::new();

    macro_rules! check {
        ($cli:ident::$fn:ident, $req:ident, $id:literal, $path:expr, $body:expr) => {{
            let mut body = $body;
            let mut prev = Value::Null;
            for round in 0..2 {
                if round == 1 {
                    if body.get("page").is_some() {
                        body["page"]["cursor"] =
                            json!(prev["page"]["nextCursor"].as_str().unwrap_or(""));
                    }
                    if body.get("sinceSeq").is_some() {
                        body["sinceSeq"] = json!(prev["nextSeq"].as_str().unwrap_or("0"));
                    }
                }
                let req: pb::$req = serde_json::from_value(body.clone()).unwrap();
                let q: Vec<String> = body
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| {
                        if k == "page" {
                            format!("page.limit=1&page.cursor={}", v["cursor"].as_str().unwrap())
                        } else {
                            format!(
                                "{k}={}",
                                v.as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| v.to_string())
                            )
                        }
                    })
                    .collect();
                let actual = grpc_json($cli::new(ch.clone()).$fn(tonic::Request::new(req)).await);
                let expected = ok_get(&app, &format!("{}?{}", $path, q.join("&"))).await;
                assert_eq!(actual, expected, "{} round {round}", $id);
                if $id == "Session.ListSessions" {
                    assert_eq!(actual["sessions"].as_array().unwrap().len(), 1);
                    if round == 0 {
                        assert!(actual["page"]["hasMore"].as_bool().unwrap());
                    } else {
                        assert_ne!(actual["sessions"], prev["sessions"]);
                    }
                }
                prev = actual;
            }
            covered.insert($id);
        }};
    }

    let p = json!({"limit": 1, "cursor": ""});
    let dp = json!({"directory": dir, "page": p});
    let d = json!({"directory": dir});
    let sd = json!({"session": sess, "directory": dir});
    check!(
        SessionClient::list_sessions,
        ListSessionsRequest,
        "Session.ListSessions",
        "/v1/sessions",
        json!({"page": p})
    );
    check!(
        AgentModelsClient::list_agent_models,
        ListAgentModelsRequest,
        "AgentModels.ListAgentModels",
        "/v1/agent-models",
        sd.clone()
    );
    check!(
        CatalogClient::list_agents,
        ListAgentsRequest,
        "Catalog.ListAgents",
        "/v1/agents",
        dp.clone()
    );
    check!(
        AuthClient::list_provider_auth,
        ListProviderAuthRequest,
        "Auth.ListProviderAuth",
        "/v1/auth",
        json!({})
    );
    check!(
        BundleApiClient::list_bundle_apis,
        ListBundleApisRequest,
        "BundleApi.ListBundleApis",
        "/v1/bundle-apis",
        sd.clone()
    );
    check!(
        CatalogClient::list_bundles,
        ListBundlesRequest,
        "Catalog.ListBundles",
        "/v1/bundles",
        d.clone()
    );
    check!(
        CatalogClient::list_commands,
        ListCommandsRequest,
        "Catalog.ListCommands",
        "/v1/commands",
        dp.clone()
    );
    check!(
        FilesClient::list_directory,
        ListDirectoryRequest,
        "Files.ListDirectory",
        "/v1/fs/list",
        d.clone()
    );
    check!(
        InteractionsClient::list_interactions,
        ListInteractionsRequest,
        "Interactions.ListInteractions",
        "/v1/interactions",
        json!({"session": sess, "directory": dir, "page": p})
    );
    check!(
        AgentModelsClient::list_model_effort_preferences,
        ListModelEffortPreferencesRequest,
        "AgentModels.ListModelEffortPreferences",
        "/v1/model-effort-preferences",
        json!({})
    );
    check!(
        CatalogClient::list_models,
        ListModelsRequest,
        "Catalog.ListModels",
        "/v1/models",
        dp.clone()
    );
    check!(
        CatalogClient::list_permission_modes,
        ListPermissionModesRequest,
        "Catalog.ListPermissionModes",
        "/v1/permission-modes",
        sd
    );
    check!(
        InteractionsClient::list_saved_rules,
        ListSavedRulesRequest,
        "Interactions.ListSavedRules",
        "/v1/permissions/rules",
        dp.clone()
    );
    check!(
        ProjectClient::list_projects,
        ListProjectsRequest,
        "Project.ListProjects",
        "/v1/projects",
        json!({"page": p})
    );
    check!(
        ProjectClient::list_project_directories,
        ListProjectDirectoriesRequest,
        "Project.ListProjectDirectories",
        format!("/v1/projects/{proj}/directories"),
        json!({"project": proj})
    );
    check!(
        CatalogClient::list_providers,
        ListProvidersRequest,
        "Catalog.ListProviders",
        "/v1/providers",
        dp.clone()
    );
    check!(
        PtyClient::list_shells,
        ListShellsRequest,
        "Pty.ListShells",
        "/v1/pty/shells",
        json!({})
    );
    check!(
        EventsClient::list_events,
        ListEventsRequest,
        "Events.ListEvents",
        format!("/v1/sessions/{sess}/events"),
        json!({"session": sess, "limit": 1, "sinceSeq": "0"})
    );
    check!(
        MessagesClient::list_messages,
        ListMessagesRequest,
        "Messages.ListMessages",
        format!("/v1/sessions/{sess}/messages"),
        json!({"session": sess, "page": p})
    );
    check!(
        CatalogClient::list_skills,
        ListSkillsRequest,
        "Catalog.ListSkills",
        "/v1/skills",
        dp.clone()
    );
    check!(
        CatalogClient::list_tools,
        ListToolsRequest,
        "Catalog.ListTools",
        "/v1/tools",
        dp.clone()
    );
    check!(
        CatalogClient::list_tui_extensions,
        ListTuiExtensionsRequest,
        "Catalog.ListTuiExtensions",
        "/v1/tui-extensions",
        d.clone()
    );
    check!(
        WorkflowClient::list_workflows,
        ListWorkflowsRequest,
        "Workflow.ListWorkflows",
        "/v1/workflows",
        dp
    );
    check!(
        WorktreesClient::list_worktrees,
        ListWorktreesRequest,
        "Worktrees.ListWorktrees",
        "/v1/worktrees",
        d
    );

    let catalog: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../packages/hya-tui/src/operations.json"
    ))
    .unwrap();
    let expected: std::collections::BTreeSet<_> = catalog
        .iter()
        .filter_map(|op| op["operationId"].as_str())
        .filter(|id| id.split('.').nth(1).unwrap().starts_with("List"))
        .collect();
    assert_eq!(covered, expected);
}

#[tokio::test]
async fn one_server_and_extra_listener_share_http_grpc_state() {
    let gate = Arc::new(Semaphore::new(0));
    let state = state_with_provider(GatedProvider::new(Arc::clone(&gate)))
        .await
        .with_scratch_root(support::tempdir("one-server-scratch"));
    let server = build(state);
    let (main, _m) = serve_server(&server, std::future::pending()).await;
    let (extra, _e) = serve_server(&server, std::future::pending()).await;

    assert_eq!(
        tcp_ok(main, "GET", "/v1/health", None).await["ok"],
        json!(true)
    );
    let connect = |addr: SocketAddr| async move {
        tonic::transport::Channel::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap()
    };
    let (main_ch, extra_ch) = (connect(main).await, connect(extra).await);
    assert!(
        pb::process_client::ProcessClient::new(main_ch.clone())
            .get_health(pb::GetHealthRequest::default())
            .await
            .unwrap()
            .into_inner()
            .ok
    );
    assert!(
        pb::relay_control_client::RelayControlClient::new(main_ch.clone())
            .get_relay_status(pb::GetRelayStatusRequest::default())
            .await
            .is_ok()
    );

    let wd = std::env::temp_dir().to_string_lossy().into_owned();
    let req = pb::CreateSessionRequest {
        agent: "build".into(),
        model: "fake".into(),
        workdir: Some(wd.clone()),
        ..Default::default()
    };
    let grpc_id = pb::session_client::SessionClient::new(main_ch)
        .create_session(req)
        .await
        .unwrap()
        .into_inner()
        .session
        .unwrap()
        .id;
    let _ = tcp_ok(main, "GET", &format!("/v1/sessions/{grpc_id}"), None).await;

    let project = id_str(
        &tcp_ok(
            main,
            "POST",
            "/v1/projects",
            Some(json!({"name": "p", "roots": [wd]})),
        )
        .await["id"],
    );
    let session = id_str(
        &tcp_ok(
            main,
            "POST",
            "/v1/sessions",
            Some(json!({"agent": "build", "model": "fake", "projectId": project})),
        )
        .await["session"]["id"],
    );
    let got = pb::session_client::SessionClient::new(extra_ch.clone())
        .get_session(pb::GetSessionRequest {
            session: session.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.id, session);

    let dir = support::tempdir("one-server-pty");
    let pty_id = id_str(
        &tcp_ok(
            main,
            "POST",
            "/v1/pty",
            Some(json!({"shell": "/bin/sh", "cwd": dir.to_string_lossy()})),
        )
        .await["id"],
    );
    assert_eq!(
        pb::pty_client::PtyClient::new(extra_ch.clone())
            .get_pty(pb::GetPtyRequest { id: pty_id.clone() })
            .await
            .unwrap()
            .into_inner()
            .id,
        pty_id
    );
    let _ = tcp_ok(main, "DELETE", &format!("/v1/pty/{pty_id}"), None).await;

    let _ = tcp_ok(
        main,
        "POST",
        &format!("/v1/sessions/{session}/turns"),
        Some(json!({"prompt": {"text": "work"}})),
    )
    .await;
    let mut projects = pb::project_client::ProjectClient::new(extra_ch);
    let get_proj = pb::GetProjectRequest { project };
    assert!(
        projects
            .get_project(get_proj.clone())
            .await
            .unwrap()
            .into_inner()
            .busy
    );
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while projects
            .get_project(get_proj.clone())
            .await
            .unwrap()
            .into_inner()
            .busy
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("busy clears on extra listener");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn one_state_runs_the_background_drivers_once() {
    let gate = Arc::new(Semaphore::new(0));
    let app = state_with_provider(GatedProvider::new(Arc::clone(&gate)))
        .await
        .with_scratch_root(support::tempdir("one-drivers-scratch"));
    let (_r1, _g, server, _r2) = (
        router(app.clone()),
        V1Grpc::new(app.clone()),
        build(app.clone()),
        router(app),
    );
    let (addr, _task) = serve_server(&server, std::future::pending()).await;

    let wd = std::env::temp_dir().to_string_lossy().into_owned();
    let project = id_str(
        &tcp_ok(
            addr,
            "POST",
            "/v1/projects",
            Some(json!({"name": "p", "roots": [wd]})),
        )
        .await["id"],
    );
    let session = id_str(
        &tcp_ok(
            addr,
            "POST",
            "/v1/sessions",
            Some(json!({"agent": "build", "model": "fake", "projectId": project})),
        )
        .await["session"]["id"],
    );

    let resp = http_conn(addr)
        .await
        .send_request(http_req(
            addr,
            "GET",
            "/v1/events/stream?interactionsOnly=true",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut global = resp.into_body();
    let is_proj = |f: &Value| f["event"]["projectsUpdated"].is_object();

    let turn = id_str(
        &tcp_ok(
            addr,
            "POST",
            &format!("/v1/sessions/{session}/turns"),
            Some(json!({"prompt": {"text": "work"}})),
        )
        .await["turn"]["id"],
    );
    assert_eq!(
        wait_tcp_notice_and_quiet(&mut global, is_proj, Duration::from_millis(60)).await,
        1
    );
    gate.add_permits(1);
    let _ = tcp_ok(
        addr,
        "POST",
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        None,
    )
    .await;
    assert_eq!(
        wait_tcp_notice_and_quiet(&mut global, is_proj, Duration::from_millis(60)).await,
        1
    );
}

struct EmptyListControl;
impl AgentModelControl for EmptyListControl {
    fn available(&self) -> bool {
        true
    }
    fn configuration_available(&self) -> bool {
        false
    }
    fn list(
        &self,
        _: TurnBinding,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn set(
        &self,
        _: TurnBinding,
        _: String,
        _: Option<AgentModelIdentity>,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        Box::pin(async { Err(AgentModelControlError::unavailable()) })
    }
    fn save_configuration(
        &self,
        _: TurnBinding,
        _: String,
        _: Option<AgentModelIdentity>,
        _: ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        Box::pin(async { Err(AgentModelControlError::unavailable()) })
    }
    fn list_model_effort_preferences(
        &self,
    ) -> AgentModelControlFuture<'_, Vec<hya_store::ModelEffortPreference>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn set_model_effort_preference(
        &self,
        _: String,
        _: String,
        _: String,
    ) -> AgentModelControlFuture<'_, ()> {
        Box::pin(async { Err(AgentModelControlError::unavailable()) })
    }
}
impl ProviderControl for EmptyListControl {
    fn available(&self) -> bool {
        false
    }
    fn list_saved_keys(&self) -> ProviderControlFuture<'_, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn list_settings(&self) -> ProviderControlFuture<'_, Vec<ProviderSettings>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn set_key(&self, _: String, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
    fn remove_key(&self, _: String) -> ProviderControlFuture<'_, (bool, ProviderChange)> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
    fn upsert_provider(&self, _: ProviderUpsert) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
    fn refresh_provider(&self, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
    fn set_model(
        &self,
        _: String,
        _: String,
        _: ProviderModelOverride,
    ) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
    fn remove_model(&self, _: String, _: String) -> ProviderControlFuture<'_, ProviderChange> {
        Box::pin(async { Err(ProviderControlError::unavailable()) })
    }
}
impl BundleControl for EmptyListControl {
    fn list(&self, _: Option<PathBuf>) -> BundleControlFuture<'_, Vec<BundleListing>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn install(&self, _: PathBuf, _: Option<PathBuf>, _: bool) -> BundleControlFuture<'_, String> {
        panic!("unused")
    }
    fn uninstall(&self, _: String, _: Option<PathBuf>) -> BundleControlFuture<'_, ()> {
        panic!("unused")
    }
    fn set_enabled(&self, _: String, _: bool) -> BundleControlFuture<'_, ()> {
        panic!("unused")
    }
}
