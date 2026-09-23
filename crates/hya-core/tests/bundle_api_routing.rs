//! Bundle API routing: template matching with path parameters, 404/405
//! decisions per scope, body/status passthrough, and reply validation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::{
    ApiMethod, ApiPathTemplate, ApiScope, BundleApiCall, BundleApiError, BundleApiProvider,
    BundleApiReply, BundleApiRequest, MAX_BUNDLE_API_BODY_BYTES, RuntimeSource, RuntimeSourceId,
    SourceApi, SourceApis,
};
use hya_proto::SessionId;
use serde_json::{Value, json};

/// Records every forwarded request; answers `reply` (or fails when `None`).
struct Recorder {
    seen: Mutex<Vec<BundleApiRequest>>,
    reply: Option<BundleApiReply>,
}

#[async_trait]
impl BundleApiProvider for Recorder {
    async fn request(&self, request: BundleApiRequest) -> Result<BundleApiReply, String> {
        self.seen.lock().unwrap().push(request);
        self.reply
            .clone()
            .ok_or_else(|| "plugin connection closed".to_string())
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

fn source(reply: Option<BundleApiReply>) -> (RuntimeSource, Arc<Recorder>) {
    let recorder = Arc::new(Recorder {
        seen: Mutex::new(Vec::new()),
        reply,
    });
    let source = RuntimeSource::new(
        RuntimeSourceId::bundle("acme/api"),
        [1; 32],
        Arc::new(()),
        Vec::new(),
    )
    .with_apis(
        vec![
            api("usage", ApiMethod::Get, ApiScope::Session, "/usage"),
            api("get-item", ApiMethod::Get, ApiScope::Global, "/items/{id}"),
            api("put-item", ApiMethod::Put, ApiScope::Global, "/items/{id}"),
            api("latest", ApiMethod::Get, ApiScope::Global, "/latest"),
        ],
        recorder.clone(),
    );
    (source, recorder)
}

fn apis(source: &RuntimeSource) -> &SourceApis {
    source.apis().expect("apis attached")
}

fn call(method: ApiMethod, session: Option<SessionId>, path: &str, body: Value) -> BundleApiCall {
    BundleApiCall {
        bundle: "acme/api".into(),
        method,
        session,
        path: path.into(),
        query: BTreeMap::from([("q".to_string(), "1".to_string())]),
        body,
    }
}

fn ok(status: u16, body: Value) -> Option<BundleApiReply> {
    Some(BundleApiReply { status, body })
}

#[tokio::test]
async fn routes_bind_decoded_params_and_pass_body_and_status_through() {
    let (source, recorder) = source(ok(201, json!({"stored": true})));
    let outcome = apis(&source)
        .invoke(call(
            ApiMethod::Put,
            None,
            "/items/a%2Fb",
            json!({"value": 1}),
        ))
        .await
        .unwrap();
    assert_eq!(outcome.api, "put-item");
    assert_eq!(outcome.status, 201);
    assert_eq!(outcome.body, json!({"stored": true}));
    let seen = recorder.seen.lock().unwrap();
    assert_eq!(seen[0].api, "put-item");
    assert_eq!(seen[0].method, ApiMethod::Put);
    assert_eq!(seen[0].path, "/items/a%2Fb", "the raw path is forwarded");
    assert_eq!(
        seen[0].path_params,
        BTreeMap::from([("id".to_string(), "a/b".to_string())])
    );
    assert_eq!(seen[0].query["q"], "1");
    assert_eq!(seen[0].body, json!({"value": 1}));
    assert_eq!(seen[0].session, None, "global requests carry no session");
}

#[tokio::test]
async fn session_scope_routes_only_session_endpoints() {
    let session = SessionId::new();
    let (source, recorder) = source(ok(200, json!([])));
    apis(&source)
        .invoke(call(ApiMethod::Get, Some(session), "/usage", Value::Null))
        .await
        .unwrap();
    assert_eq!(recorder.seen.lock().unwrap()[0].session, Some(session));
    // `/usage` is a session endpoint: the global mount does not serve it,
    // and `/latest` is global only.
    for (session, path) in [(None, "/usage"), (Some(session), "/latest")] {
        let error = apis(&source)
            .invoke(call(ApiMethod::Get, session, path, Value::Null))
            .await
            .unwrap_err();
        assert!(matches!(error, BundleApiError::NotFound { .. }), "{error}");
    }
}

#[tokio::test]
async fn method_mismatch_lists_the_allowed_methods() {
    let (source, _) = source(ok(200, Value::Null));
    let error = apis(&source)
        .invoke(call(ApiMethod::Delete, None, "/items/7", Value::Null))
        .await
        .unwrap_err();
    match error {
        BundleApiError::MethodNotAllowed { allow, .. } => {
            assert_eq!(allow, [ApiMethod::Get, ApiMethod::Put]);
        }
        other => panic!("expected 405: {other}"),
    }
    for path in ["/items/", "/items/7/x", "/nothing"] {
        let error = apis(&source)
            .invoke(call(ApiMethod::Get, None, path, Value::Null))
            .await
            .unwrap_err();
        assert!(matches!(error, BundleApiError::NotFound { .. }), "{path}");
    }
}

#[tokio::test]
async fn malformed_calls_are_bad_requests_before_the_process_is_asked() {
    let (source, recorder) = source(ok(200, Value::Null));
    let error = apis(&source)
        .invoke(call(ApiMethod::Get, None, "/items/%zz", Value::Null))
        .await
        .unwrap_err();
    assert!(matches!(error, BundleApiError::BadRequest(_)), "{error}");
    let big = Value::String("x".repeat(MAX_BUNDLE_API_BODY_BYTES));
    let error = apis(&source)
        .invoke(call(ApiMethod::Put, None, "/items/1", big))
        .await
        .unwrap_err();
    assert!(matches!(error, BundleApiError::BadRequest(_)), "{error}");
    assert!(recorder.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn process_failures_and_malformed_replies_fail_the_call() {
    for reply in [
        None,
        ok(199, Value::Null),
        ok(600, Value::Null),
        ok(204, json!({"unexpected": true})),
    ] {
        let (source, _) = source(reply.clone());
        let error = apis(&source)
            .invoke(call(ApiMethod::Get, None, "/latest", Value::Null))
            .await
            .unwrap_err();
        assert!(
            matches!(error, BundleApiError::Failed { ref api, .. } if api == "latest"),
            "{reply:?}: {error}"
        );
    }
    let (source, _) = source(ok(204, Value::Null));
    let outcome = apis(&source)
        .invoke(call(ApiMethod::Get, None, "/latest", Value::Null))
        .await
        .unwrap();
    assert_eq!((outcome.status, outcome.body), (204, Value::Null));
}
