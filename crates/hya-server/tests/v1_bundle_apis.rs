//! Bundle-registered endpoints: `GET /v1/bundle-apis`, the session-scoped
//! `/v1/sessions/{session}/bundles/{bundle}/{path}` and global
//! `/v1/bundles/{bundle}/api/{path}` passthrough routes (all five methods),
//! stable error codes, and the gRPC binding.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_api::v1::bundle_api_server::BundleApi as _;
use hya_core::{
    AgentSpec, ApiMethod, ApiPathTemplate, ApiScope, BundleApiProvider, BundleApiReply,
    BundleApiRequest, EventBus, RuntimeSource, RuntimeSourceId, SessionEngine, SourceApi,
};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const BUNDLE: &str = "hya/server-tests-api-user";
const BUNDLE_SEGMENT: &str = "hya%2Fserver-tests-api-user";

/// Echoes the routed request. The `status` query parameter picks the reply
/// status; endpoint `broken` fails like a crashed process; endpoint `empty`
/// answers 204 with no body.
struct Echo;

#[async_trait]
impl BundleApiProvider for Echo {
    async fn request(&self, request: BundleApiRequest) -> Result<BundleApiReply, String> {
        match request.api.as_str() {
            "broken" => return Err("plugin connection closed".to_string()),
            "empty" => {
                return Ok(BundleApiReply {
                    status: 204,
                    body: Value::Null,
                });
            }
            _ => {}
        }
        let status = request
            .query
            .get("status")
            .and_then(|status| status.parse().ok())
            .unwrap_or(200);
        Ok(BundleApiReply {
            status,
            body: json!({
                "api": request.api,
                "method": request.method.as_str(),
                "path": request.path,
                "path_params": request.path_params,
                "query": request.query,
                "body": request.body,
                "session": request.session,
                "big": 12_345_678_901_u64,
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

async fn state() -> AppState {
    let providers =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(Vec::new()))));
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = support::runtime_with_catalog(
        tools,
        &[
            support::AgentFixture::main("build"),
            support::AgentFixture::subagent("api-user"),
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
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::bundle(BUNDLE),
                    [7; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_apis(apis, Arc::new(Echo)),
            ])
        })
        .unwrap();
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
}

struct Reply {
    status: StatusCode,
    content_type: Option<String>,
    allow: Option<String>,
    raw: Vec<u8>,
    json: Value,
}

async fn send_raw(app: axum::Router, method: Method, uri: &str, body: Body) -> Reply {
    let resp = app
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
    let text = |name| {
        resp.headers()
            .get(name)
            .map(|value: &header::HeaderValue| value.to_str().unwrap().to_string())
    };
    let content_type = text(header::CONTENT_TYPE);
    let allow = text(header::ALLOW);
    let raw = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        content_type,
        allow,
        json: serde_json::from_slice(&raw).unwrap_or(Value::Null),
        raw,
    }
}

async fn send(app: axum::Router, method: Method, uri: &str, body: Value) -> Reply {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    send_raw(app, method, uri, body).await
}

async fn create_session(app: &axum::Router) -> String {
    let reply = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": std::env::temp_dir().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.json);
    reply.json["session"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn bundle_apis_are_listed_with_their_schemas() {
    let app = router(state().await);
    let reply = send(app, Method::GET, "/v1/bundle-apis", Value::Null).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.json);
    let apis = reply.json["apis"].as_array().unwrap();
    let ids = apis
        .iter()
        .map(|api| api["api"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
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
    let usage = apis.iter().find(|api| api["api"] == "usage").unwrap();
    assert_eq!(
        *usage,
        json!({
            "bundle": BUNDLE,
            "api": "usage",
            "method": "GET",
            "scope": "session",
            "path": "/usage",
            "description": "Token usage",
            "responseSchema": {"type": "object"},
        })
    );
}

#[tokio::test]
async fn session_endpoint_answers_the_process_status_and_body_verbatim() {
    let app = router(state().await);
    let session = create_session(&app).await;
    let reply = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}/bundles/{BUNDLE_SEGMENT}/usage?scope=tree&status=207"),
        Value::Null,
    )
    .await;
    assert_eq!(reply.status.as_u16(), 207, "{}", reply.json);
    assert_eq!(reply.content_type.as_deref(), Some("application/json"));
    let body = &reply.json;
    assert_eq!(body["api"], "usage", "no envelope around the process body");
    assert_eq!(body["session"], session);
    assert_eq!(body["path"], "/usage");
    assert_eq!(body["query"], json!({"scope": "tree", "status": "207"}));
    assert_eq!(body["body"], Value::Null);
    assert_eq!(
        body["big"].as_u64(),
        Some(12_345_678_901),
        "HTTP bodies keep integers exact"
    );
}

#[tokio::test]
async fn global_endpoints_serve_all_five_methods_with_bodies_and_params() {
    let app = router(state().await);
    for method in ApiMethod::ALL {
        let reply = send(
            app.clone(),
            Method::from_bytes(method.as_str().as_bytes()).unwrap(),
            &format!("/v1/bundles/{BUNDLE_SEGMENT}/api/items/a%2Fb?status=201"),
            json!({"value": method.as_str()}),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::CREATED,
            "{method}: {}",
            reply.json
        );
        let body = &reply.json;
        assert_eq!(
            body["api"],
            format!("item-{}", method.as_str().to_lowercase())
        );
        assert_eq!(body["method"], method.as_str());
        assert_eq!(body["path"], "/items/a%2Fb", "the raw path is forwarded");
        assert_eq!(body["path_params"], json!({"id": "a/b"}));
        assert_eq!(body["body"], json!({"value": method.as_str()}));
        assert_eq!(
            body["session"],
            Value::Null,
            "global requests carry no session"
        );
    }
    let reply = send(
        app,
        Method::DELETE,
        &format!("/v1/bundles/{BUNDLE_SEGMENT}/api/empty"),
        Value::Null,
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.raw.is_empty(), "a null body sends no content");
    assert_eq!(reply.content_type, None);
}

#[tokio::test]
async fn host_side_failures_use_the_stable_error_codes() {
    let app = router(state().await);
    let session = create_session(&app).await;
    let session_base = format!("/v1/sessions/{session}/bundles/{BUNDLE_SEGMENT}");
    let global_base = format!("/v1/bundles/{BUNDLE_SEGMENT}/api");
    for (method, uri, status, code) in [
        (
            Method::GET,
            format!(
                "/v1/sessions/{}/bundles/{BUNDLE_SEGMENT}/usage",
                SessionId::new()
            ),
            StatusCode::NOT_FOUND,
            "session_not_found",
        ),
        (
            Method::GET,
            format!("/v1/sessions/{session}/bundles/hya%2Fmissing/usage"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{session_base}/missing"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{global_base}/usage"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::GET,
            format!("{global_base}/items/"),
            StatusCode::NOT_FOUND,
            "bundle_api_not_found",
        ),
        (
            Method::POST,
            format!("{session_base}/usage"),
            StatusCode::METHOD_NOT_ALLOWED,
            "bundle_api_method_not_allowed",
        ),
        (
            Method::GET,
            format!("{global_base}/items/%zz"),
            StatusCode::BAD_REQUEST,
            "bundle_api_bad_request",
        ),
        (
            Method::GET,
            format!("{session_base}/broken"),
            StatusCode::BAD_GATEWAY,
            "bundle_api_failed",
        ),
    ] {
        let reply = send(app.clone(), method.clone(), &uri, Value::Null).await;
        assert_eq!(reply.status, status, "{method} {uri}: {}", reply.json);
        assert_eq!(reply.json["error"]["code"], code, "{method} {uri}");
    }
    let reply = send(
        app.clone(),
        Method::POST,
        &format!("{session_base}/usage"),
        Value::Null,
    )
    .await;
    assert_eq!(
        reply.allow.as_deref(),
        Some("GET"),
        "405 lists the allowed methods"
    );

    let reply = send_raw(
        app.clone(),
        Method::POST,
        &format!("{global_base}/items/1"),
        Body::from("not json"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json["error"]["code"], "bundle_api_bad_request");

    let oversized = format!("\"{}\"", "x".repeat(hya_core::MAX_BUNDLE_API_BODY_BYTES));
    let reply = send_raw(
        app,
        Method::POST,
        &format!("{global_base}/items/1"),
        Body::from(oversized),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.json["error"]["code"], "bundle_api_bad_request");
}

#[tokio::test]
async fn grpc_invokes_bundle_apis_with_status_in_the_response() {
    let state = state().await;
    let app = router(state.clone());
    let session = create_session(&app).await;
    let grpc = V1Grpc::new(state);

    let reply = grpc
        .invoke_session_bundle_api(tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session: session.clone(),
            bundle: BUNDLE.into(),
            method: "GET".into(),
            path: "/usage".into(),
            query: [("status".to_string(), "404".to_string())].into(),
            body: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.bundle, BUNDLE);
    assert_eq!(reply.api, "usage");
    assert_eq!(
        reply.status, 404,
        "a process status is data, not a gRPC error"
    );
    assert_eq!(reply.content_type, "application/json");
    let body = serde_json::to_value(reply.body.unwrap()).unwrap();
    assert_eq!(body["session"], session);

    let reply = grpc
        .invoke_global_bundle_api(tonic::Request::new(pb::InvokeGlobalBundleApiRequest {
            bundle: BUNDLE.into(),
            method: "PATCH".into(),
            path: "/items/7".into(),
            query: Default::default(),
            body: Some(serde_json::from_value(json!({"n": 1})).unwrap()),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(reply.api, "item-patch");
    assert_eq!(reply.status, 200);
    let body = serde_json::to_value(reply.body.unwrap()).unwrap();
    assert_eq!(body["body"], json!({"n": 1.0}));
    assert_eq!(body["path_params"], json!({"id": "7"}));

    let empty = grpc
        .invoke_global_bundle_api(tonic::Request::new(pb::InvokeGlobalBundleApiRequest {
            bundle: BUNDLE.into(),
            method: "DELETE".into(),
            path: "/empty".into(),
            query: Default::default(),
            body: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!((empty.status, empty.content_type.as_str()), (204, ""));
    assert!(empty.body.is_none());

    let listed = grpc
        .list_bundle_apis(tonic::Request::new(pb::ListBundleApisRequest::default()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.apis.len(), 8);

    for (method, path, code) in [
        ("GET", "/missing", tonic::Code::NotFound),
        ("POST", "/latest-nope", tonic::Code::NotFound),
        ("DELETE", "/usage", tonic::Code::NotFound),
        ("HEAD", "/items/1", tonic::Code::InvalidArgument),
        ("POST", "/empty", tonic::Code::Unimplemented),
    ] {
        let error = grpc
            .invoke_global_bundle_api(tonic::Request::new(pb::InvokeGlobalBundleApiRequest {
                bundle: BUNDLE.into(),
                method: method.into(),
                path: path.into(),
                query: Default::default(),
                body: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), code, "{method} {path}: {error:?}");
    }
    let error = grpc
        .invoke_session_bundle_api(tonic::Request::new(pb::InvokeSessionBundleApiRequest {
            session,
            bundle: BUNDLE.into(),
            method: "GET".into(),
            path: "/broken".into(),
            query: Default::default(),
            body: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unavailable);
}
