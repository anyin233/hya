//! Bundle-registered HTTP/gRPC passthrough APIs (`/v2/bundle-apis`,
//! `/v2/sessions/{session}/bundles/{bundle}/{path}`,
//! `/v2/bundles/{bundle}/api/{path}`) and the `/v2/skills` + `/v2/commands`
//! catalog with diagnostics and bundle-skill expansion.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v2 as pb;
use hya_api::v2::bundle_api_server::BundleApi as _;
use hya_engine::{
    ApiMethod, ApiPathTemplate, ApiScope, BundleApiProvider, BundleApiReply, BundleApiRequest,
    EventBus, RuntimeSource, RuntimeSourceId, SessionEngine, SourceApi,
};
use hya_proto::SessionId;
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, V2Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::harness::{
    TextProvider, create_session, enc, get_json, id_str, ok_get, ok_post, start_turn, state_of,
    wait_turn,
};
use crate::support::{self, AgentFixture};

// ---- Bundle APIs ----

const BUNDLE: &str = "hya/server-tests-api-user";
const BUNDLE_SEGMENT: &str = "hya%2Fserver-tests-api-user";

struct EchoApi;

#[async_trait]
impl BundleApiProvider for EchoApi {
    async fn request(&self, req: BundleApiRequest) -> Result<BundleApiReply, String> {
        match req.api.as_str() {
            "broken" => return Err("plugin connection closed".into()),
            "empty" => {
                return Ok(BundleApiReply {
                    status: 204,
                    body: Value::Null,
                });
            }
            _ => {}
        }
        let status = req
            .query
            .get("status")
            .and_then(|s| s.parse().ok())
            .unwrap_or(200);
        Ok(BundleApiReply {
            status,
            body: json!({
                "api": req.api, "method": req.method.as_str(), "path": req.path,
                "path_params": req.path_params, "query": req.query,
                "body": req.body, "session": req.session, "big": 12_345_678_901_u64,
            }),
        })
    }
}

fn api(id: &str, method: ApiMethod, scope: ApiScope, path: &str) -> SourceApi {
    SourceApi {
        id: id.into(),
        method,
        scope,
        path: ApiPathTemplate::parse(path).unwrap(),
        description: String::new(),
        request_schema: None,
        response_schema: None,
    }
}

async fn bundle_api_state() -> AppState {
    let runtime = support::runtime_with_catalog(
        Arc::new(hya_tool_loader::builtins()),
        &[
            AgentFixture::main("build"),
            AgentFixture::subagent("api-user"),
        ],
    );
    let mut usage = api("usage", ApiMethod::Get, ApiScope::Session, "/usage");
    usage.description = "Token usage".into();
    usage.response_schema = Some(json!({"type": "object"}));
    let mut apis = vec![
        usage,
        api("broken", ApiMethod::Get, ApiScope::Session, "/broken"),
        api("empty", ApiMethod::Delete, ApiScope::Global, "/empty"),
    ];
    for method in ApiMethod::ALL {
        apis.push(api(
            &format!("item-{}", method.as_str().to_lowercase()),
            method,
            ApiScope::Global,
            "/items/{id}",
        ));
    }
    runtime
        .refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle(BUNDLE),
                    [7; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_apis(apis, Arc::new(EchoApi)),
            ])
        })
        .unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(
                ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(Vec::new()))),
            ),
            runtime,
            perm,
            EventBus::default(),
        )
        .with_legacy_coordination(),
    );
    state_of(engine, std::env::temp_dir())
}

struct RawReply {
    status: StatusCode,
    content_type: Option<String>,
    allow: Option<String>,
    raw: Vec<u8>,
    json: Value,
}

async fn send_body(app: &axum::Router, method: Method, uri: &str, body: Body) -> RawReply {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let hdr = |n| {
        resp.headers()
            .get(n)
            .map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned())
    };
    let (content_type, allow) = (hdr(header::CONTENT_TYPE), hdr(header::ALLOW));
    let raw = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    RawReply {
        status,
        content_type,
        allow,
        json: serde_json::from_slice(&raw).unwrap_or(Value::Null),
        raw,
    }
}

#[tokio::test]
async fn bundle_apis_list_session_and_global_passthrough_all_methods() {
    let app = router(bundle_api_state().await);
    let listed = ok_get(&app, "/v2/bundle-apis").await;
    let apis = listed["apis"].as_array().unwrap();
    let fixture_apis: Vec<&str> = apis
        .iter()
        .filter(|a| a["bundle"] == json!(BUNDLE))
        .map(|a| a["api"].as_str().unwrap())
        .collect();
    assert_eq!(
        fixture_apis,
        [
            "broken",
            "empty",
            "item-delete",
            "item-get",
            "item-patch",
            "item-post",
            "item-put",
            "usage"
        ]
    );
    let multi_agent_apis: Vec<&str> = apis
        .iter()
        .filter(|a| a["bundle"] == json!("hya/multi-agent"))
        .map(|a| a["api"].as_str().unwrap())
        .collect();
    assert_eq!(
        multi_agent_apis,
        [
            "archive_subagent",
            "archive_subagent_global",
            "list_members",
            "list_members_global",
            "steer_subagent",
            "steer_subagent_global",
        ]
    );
    assert_eq!(
        apis.iter().find(|a| a["api"] == "usage").unwrap(),
        &json!({
            "bundle": BUNDLE, "api": "usage", "method": "GET", "scope": "session",
            "path": "/usage", "description": "Token usage", "responseSchema": {"type": "object"},
        })
    );

    // Session endpoint preserves status, exact u64, and query verbatim.
    let session = create_session(&app, &std::env::temp_dir()).await;
    let reply = send_body(
        &app,
        Method::GET,
        &format!("/v2/sessions/{session}/bundles/{BUNDLE_SEGMENT}/usage?scope=tree&status=207"),
        Body::empty(),
    )
    .await;
    assert_eq!(
        (reply.status.as_u16(), reply.content_type.as_deref()),
        (207, Some("application/json"))
    );
    assert_eq!(
        (
            &reply.json["api"],
            &reply.json["session"],
            &reply.json["path"]
        ),
        (&json!("usage"), &json!(session), &json!("/usage"))
    );
    assert_eq!(reply.json["big"].as_u64(), Some(12_345_678_901));

    // Global endpoints support all five HTTP methods + 204 empty response.
    for method in ApiMethod::ALL {
        let reply = send_body(
            &app,
            Method::from_bytes(method.as_str().as_bytes()).unwrap(),
            &format!("/v2/bundles/{BUNDLE_SEGMENT}/api/items/a%2Fb?status=201"),
            Body::from(json!({"value": method.as_str()}).to_string()),
        )
        .await;
        assert_eq!(reply.status, StatusCode::CREATED);
        assert_eq!(
            reply.json["api"],
            format!("item-{}", method.as_str().to_lowercase())
        );
        assert_eq!(
            (
                &reply.json["path"],
                &reply.json["path_params"],
                &reply.json["session"]
            ),
            (&json!("/items/a%2Fb"), &json!({"id": "a/b"}), &Value::Null)
        );
    }
    let empty = send_body(
        &app,
        Method::DELETE,
        &format!("/v2/bundles/{BUNDLE_SEGMENT}/api/empty"),
        Body::empty(),
    )
    .await;
    assert_eq!(
        (empty.status, empty.raw.is_empty(), empty.content_type),
        (StatusCode::NO_CONTENT, true, None)
    );
}

#[tokio::test]
async fn bundle_api_host_errors_and_grpc_invocation() {
    let state = bundle_api_state().await;
    let app = router(state.clone());
    let session = create_session(&app, &std::env::temp_dir()).await;
    let sb = format!("/v2/sessions/{session}/bundles/{BUNDLE_SEGMENT}");
    let gb = format!("/v2/bundles/{BUNDLE_SEGMENT}/api");

    for (method, uri, status, code) in [
        (
            Method::GET,
            format!(
                "/v2/sessions/{}/bundles/{BUNDLE_SEGMENT}/usage",
                SessionId::new()
            ),
            StatusCode::NOT_FOUND,
            "session_not_found",
        ),
        (
            Method::GET,
            format!("/v2/sessions/{session}/bundles/hya%2Fmissing/usage"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{sb}/missing"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{gb}/usage"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{gb}/items/"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::POST,
            format!("{sb}/usage"),
            StatusCode::METHOD_NOT_ALLOWED,
            "bundle_api_method_not_allowed",
        ),
        (
            Method::GET,
            format!("{gb}/items/%zz"),
            StatusCode::BAD_REQUEST,
            "bundle_api_bad_request",
        ),
        (
            Method::GET,
            format!("{sb}/broken"),
            StatusCode::BAD_GATEWAY,
            "bundle_api_failed",
        ),
    ] {
        let reply = send_body(&app, method.clone(), &uri, Body::empty()).await;
        assert_eq!(
            (reply.status, reply.json["error"]["code"].as_str()),
            (status, Some(code)),
            "{method} {uri}"
        );
    }
    assert_eq!(
        send_body(&app, Method::POST, &format!("{sb}/usage"), Body::empty())
            .await
            .allow
            .as_deref(),
        Some("GET")
    );

    for body in [
        Body::from("not json"),
        Body::from(format!(
            "\"{}\"",
            "x".repeat(hya_engine::MAX_BUNDLE_API_BODY_BYTES)
        )),
    ] {
        let reply = send_body(&app, Method::POST, &format!("{gb}/items/1"), body).await;
        assert_eq!(
            (reply.status, &reply.json["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("bundle_api_bad_request"))
        );
    }

    // gRPC passthrough preserves status as data and maps host errors canonically.
    let grpc = V2Grpc::new(state);
    let g_req = |method: &str, path: &str, body: Option<pbjson_types::Value>| {
        tonic::Request::new(pb::InvokeGlobalBundleApiRequest {
            bundle: BUNDLE.into(),
            method: method.into(),
            path: path.into(),
            query: Default::default(),
            body,
        })
    };
    let reply = grpc
        .invoke_session_bundle_api(tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session: session.clone(),
            bundle: BUNDLE.into(),
            method: "GET".into(),
            path: "/usage".into(),
            query: [("status".into(), "404".into())].into(),
            body: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (
            reply.api.as_str(),
            reply.status,
            reply.content_type.as_str()
        ),
        ("usage", 404, "application/json")
    );

    let patched = grpc
        .invoke_global_bundle_api(g_req(
            "PATCH",
            "/items/7",
            Some(serde_json::from_value(json!({"n": 1})).unwrap()),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!((patched.api.as_str(), patched.status), ("item-patch", 200));

    let empty = grpc
        .invoke_global_bundle_api(g_req("DELETE", "/empty", None))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        (
            empty.status,
            empty.content_type.as_str(),
            empty.body.is_none()
        ),
        (204, "", true)
    );
    let grpc_apis = grpc
        .list_bundle_apis(tonic::Request::new(pb::ListBundleApisRequest::default()))
        .await
        .unwrap()
        .into_inner()
        .apis;
    assert_eq!(grpc_apis.iter().filter(|a| a.bundle == BUNDLE).count(), 8);
    assert_eq!(
        grpc_apis
            .iter()
            .filter(|a| a.bundle == "hya/multi-agent")
            .count(),
        6
    );

    for (method, path, code) in [
        ("GET", "/missing", tonic::Code::NotFound),
        ("DELETE", "/usage", tonic::Code::NotFound),
        ("HEAD", "/items/1", tonic::Code::InvalidArgument),
        ("POST", "/empty", tonic::Code::Unimplemented),
    ] {
        assert_eq!(
            grpc.invoke_global_bundle_api(g_req(method, path, None))
                .await
                .unwrap_err()
                .code(),
            code,
            "{method} {path}"
        );
    }
    assert_eq!(
        grpc.invoke_session_bundle_api(tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session,
            bundle: BUNDLE.into(),
            method: "GET".into(),
            path: "/broken".into(),
            query: Default::default(),
            body: None,
        }))
        .await
        .unwrap_err()
        .code(),
        tonic::Code::Unavailable
    );
}

#[tokio::test]
async fn native_bundle_apis_dispatch_uniformly_over_http_and_grpc() {
    use hya_runtime_abi::{
        HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
        NativeBundleApiRequest,
    };

    const NATIVE_BUNDLE: &str = "hya/server-tests-native-api-test";
    const NATIVE_SEG: &str = "hya%2Fserver-tests-native-api-test";

    struct NativeEcho {
        endpoints: Vec<NativeApiEndpoint>,
    }

    #[async_trait]
    impl NativeBundleApiHandler for NativeEcho {
        fn id(&self) -> &str {
            NATIVE_BUNDLE
        }

        fn endpoints(&self) -> &[NativeApiEndpoint] {
            &self.endpoints
        }

        async fn handle(
            &self,
            req: NativeBundleApiRequest,
        ) -> Result<NativeBundleApiReply, HostError> {
            Ok(NativeBundleApiReply {
                status: 200,
                body: json!({
                    "api": req.api,
                    "session": req.session.map(|s| s.to_string()),
                    "params": req.path_params,
                    "query": req.query,
                    "body": req.body,
                }),
            })
        }
    }

    let handler: Arc<dyn NativeBundleApiHandler> = Arc::new(NativeEcho {
        endpoints: vec![
            NativeApiEndpoint {
                id: "status".into(),
                method: ApiMethod::Get,
                scope: ApiScope::Global,
                path: "/status".into(),
                description: "Global status".into(),
            },
            NativeApiEndpoint {
                id: "ping".into(),
                method: ApiMethod::Post,
                scope: ApiScope::Session,
                path: "/items/{item}".into(),
                description: "Session ping".into(),
            },
        ],
    });
    let runtime = support::runtime_with_catalog(
        Arc::new(hya_tool_loader::builtins()),
        &[
            AgentFixture::main("build"),
            AgentFixture::subagent("native-api-test"),
        ],
    );
    let source = RuntimeSource::new(
        RuntimeSourceId::bundle(NATIVE_BUNDLE),
        [9; 32],
        Arc::new(()),
        Vec::new(),
    )
    .with_native_apis(vec![handler])
    .unwrap();
    runtime.refresh(|c| c.upsert_sources(vec![source])).unwrap();

    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(TextProvider))),
        runtime,
        perm,
        EventBus::default(),
    ));
    let state = state_of(engine, std::env::temp_dir());
    let app = router(state.clone());
    let grpc = V2Grpc::new(state);

    let listed = ok_get(&app, "/v2/bundle-apis").await;
    let apis = listed["apis"].as_array().unwrap();
    let native_apis: Vec<&Value> = apis
        .iter()
        .filter(|a| a["bundle"] == json!(NATIVE_BUNDLE))
        .collect();
    assert_eq!(native_apis.len(), 2);
    assert_eq!(native_apis[0]["bundle"], json!(NATIVE_BUNDLE));

    let global_http = ok_get(&app, &format!("/v2/bundles/{NATIVE_SEG}/api/status?k=v")).await;
    assert_eq!(global_http["api"], json!("status"));
    assert_eq!(global_http["query"]["k"], json!("v"));

    let session = create_session(&app, &std::env::temp_dir()).await;
    let sess_http = ok_post(
        &app,
        &format!("/v2/sessions/{session}/bundles/{NATIVE_SEG}/items/alpha"),
        json!({"ok": true}),
    )
    .await;
    assert_eq!(sess_http["api"], json!("ping"));
    assert_eq!(sess_http["params"]["item"], json!("alpha"));
    assert_eq!(sess_http["session"], json!(session));
    assert_eq!(sess_http["body"], json!({"ok": true}));

    let sess_grpc = grpc
        .invoke_session_bundle_api(tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session: session.clone(),
            bundle: NATIVE_BUNDLE.into(),
            method: "POST".into(),
            path: "/items/alpha".into(),
            query: Default::default(),
            body: Some(serde_json::from_value(json!({"ok": true})).unwrap()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(sess_grpc.status, 200);
    assert_eq!(
        serde_json::to_value(sess_grpc.body.unwrap()).unwrap(),
        sess_http
    );
}

// ---- Skills Catalog ----

async fn skill_app() -> axum::Router {
    let mut agents = support::test_agents();
    agents.push(AgentFixture::main("helper").bundle_skill("helper-guide"));
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(TextProvider))),
        support::runtime_with_catalog(Arc::new(hya_tool_loader::builtins()), &agents),
        perm,
        EventBus::default(),
    ));
    router(state_of(engine, std::env::temp_dir()))
}

fn named<'a>(rows: &'a Value, field: &str, name: &str) -> Option<&'a Value> {
    rows[field]
        .as_array()
        .and_then(|r| r.iter().find(|row| row["name"] == name))
}

fn write_rel(dir: &std::path::Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn skill_listing_reports_diagnostics_and_manual_only_skills() {
    let app = skill_app().await;
    let dir = support::tempdir("skill-diagnostics");
    write_rel(
        &dir,
        ".hya/skills/manual/SKILL.md",
        "---\nname: manual\ndescription: by hand\ndisable-model-invocation: true\n---\nbody\n",
    );
    write_rel(
        &dir,
        ".hya/skills/broken/SKILL.md",
        "---\nname: broken\n---\nno description\n",
    );
    write_rel(&dir, ".hya/commands/skill:taken.md", "reserved\n");

    let q = enc(&dir.to_string_lossy());
    let skills = ok_get(&app, &format!("/v2/skills?directory={q}")).await;
    assert_eq!(
        named(&skills, "skills", "manual").unwrap()["disableModelInvocation"],
        true
    );
    assert!(skills["diagnostics"].as_array().unwrap().iter().any(|r| {
        r["kind"] == "missing_description"
            && r["path"]
                .as_str()
                .unwrap()
                .ends_with(".hya/skills/broken/SKILL.md")
    }));

    let cmds = ok_get(&app, &format!("/v2/commands?directory={q}")).await;
    assert_eq!(
        named(&cmds, "commands", "skill:manual").unwrap()["scope"],
        "project"
    );
    assert!(
        cmds["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "reserved")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_lists_and_expands_agent_bundle_skills() {
    let app = skill_app().await;
    let dir = support::tempdir("skill-session");
    write_rel(
        &dir,
        ".hya/skills/local/SKILL.md",
        "---\nname: local\ndescription: a project skill\n---\nlocal body\n",
    );
    let session = id_str(
        &ok_post(
            &app,
            "/v2/sessions",
            json!({"agent": "helper", "model": "fake", "workdir": dir.to_string_lossy()}),
        )
        .await["session"]["id"],
    );

    let by_dir = ok_get(
        &app,
        &format!("/v2/skills?directory={}", enc(&dir.to_string_lossy())),
    )
    .await;
    assert!(
        named(&by_dir, "skills", "helper-guide").is_none()
            && named(&by_dir, "skills", "local").is_some()
    );

    let by_sess = ok_get(&app, &format!("/v2/skills?session={session}")).await;
    let guide = named(&by_sess, "skills", "helper-guide").expect("bundle skill");
    assert_eq!(
        (&guide["source"], &guide["bundleId"]),
        (&json!("bundle"), &json!("hya/server-tests-helper"))
    );
    assert!(named(&by_sess, "skills", "local").is_some());

    let cmds = ok_get(&app, &format!("/v2/commands?session={session}")).await;
    assert_eq!(
        named(&cmds, "commands", "skill:helper-guide").unwrap()["scope"],
        "bundle"
    );

    let turn = start_turn(
        &app,
        &session,
        json!({"command": {"command": "skill:helper-guide", "arguments": "go"}}),
    )
    .await;
    wait_turn(&app, &session, &turn).await;
    let msgs = ok_get(&app, &format!("/v2/sessions/{session}/messages")).await;
    assert_eq!(
        msgs["messages"][0]["parts"][0]["text"]["text"],
        json!(
            "<skill name=\"helper-guide\" location=\"bundle:hya/server-tests-helper/resources/skills/helper-guide.md\">\nhelper-guide body\n</skill>\n\ngo"
        )
    );
    assert_eq!(
        get_json(&app, &format!("/v2/skills?session={}", SessionId::new()))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let _ = std::fs::remove_dir_all(&dir);
}
