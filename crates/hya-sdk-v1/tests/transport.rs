//! HTTP transport boundaries, independent of the backend router.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{Router, extract::OriginalUri, http::StatusCode};
use hya_api::v1 as pb;
use hya_sdk_v1::{Client, SdkError, V1Sdk};
use std::time::Duration;

async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, server)
}

#[tokio::test]
async fn session_turn_and_interaction_path_segments_are_encoded() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let app = Router::new().fallback(move |uri: OriginalUri| {
        tx.send(uri.0.path().to_owned()).unwrap();
        async { axum::Json(serde_json::json!({})) }
    });
    let (base, server) = serve(app).await;
    let sdk = V1Sdk::new(&base, "");
    let client = Client::new(&base);
    let session = "session/a b?c#d%é";
    let turn = "turn/x ?#%";
    let encoded_session = "session%2Fa%20b%3Fc%23d%25%C3%A9";
    let encoded_turn = "turn%2Fx%20%3F%23%25";
    macro_rules! path {
        ($call:expr, $expected:expr) => {{
            let _ = $call.await.unwrap();
            assert_eq!(rx.recv().await.unwrap(), $expected);
        }};
    }
    let root = format!("/v1/sessions/{encoded_session}");
    path!(sdk.get_session(session), root);
    path!(
        sdk.update_session(pb::UpdateSessionRequest {
            session: session.into(),
            ..Default::default()
        }),
        root
    );
    path!(
        sdk.create_turn(
            session,
            pb::create_turn_request::Kind::Prompt(pb::PromptTurn::default())
        ),
        format!("{root}/turns")
    );
    path!(
        sdk.wait_turn(session, turn, Duration::from_millis(1)),
        format!("{root}/turns/{encoded_turn}/wait")
    );
    path!(
        sdk.cancel_turn(session, turn),
        format!("{root}/turns/{encoded_turn}/cancel")
    );
    path!(sdk.list_messages(session), format!("{root}/messages"));
    path!(sdk.todo(session), format!("{root}/todo"));
    path!(sdk.list_events(session, 0), format!("{root}/events"));
    path!(sdk.list_raw_events(session, 0), format!("{root}/events"));
    path!(sdk.workflow_state(session), format!("{root}/workflow"));
    path!(sdk.archive_subagent(session), format!("{root}/archive"));
    path!(
        sdk.steer_subagent(session, "hello"),
        format!("{root}/steer")
    );
    path!(
        sdk.submit_workflow_command(pb::SubmitWorkflowCommandRequest {
            session: session.into(),
            ..Default::default()
        }),
        format!("{root}/workflow")
    );
    path!(
        sdk.respond_interaction(
            session,
            pb::respond_interaction_request::Response::Permission(pb::PermissionResponse::default())
        ),
        format!("/v1/interactions/{encoded_session}/respond")
    );
    path!(
        sdk.stream_session(session, 0),
        format!("{root}/events/stream")
    );
    path!(
        client.respond_interaction(session, &pb::RespondInteractionRequest::default()),
        format!("/v1/interactions/{encoded_session}/respond")
    );
    path!(
        sdk.get_project(session),
        format!("/v1/projects/{encoded_session}")
    );
    path!(
        client.get_project(session),
        format!("/v1/projects/{encoded_session}")
    );
    let typed_session = hya_proto::SessionId::new();
    let root = format!("/v1/sessions/{typed_session}");
    path!(
        client.create_turn(typed_session, "hello"),
        format!("{root}/turns")
    );
    path!(
        client.wait_turn(typed_session, turn, Duration::from_millis(1)),
        format!("{root}/turns/{encoded_turn}/wait")
    );
    path!(
        client.list_events(typed_session, None, true),
        format!("{root}/events")
    );
    server.abort();
}

#[tokio::test]
async fn unstructured_errors_include_http_status() {
    for body in ["", "backend stopping", "{}"] {
        let app =
            Router::new().fallback(move || async move { (StatusCode::SERVICE_UNAVAILABLE, body) });
        let (base, server) = serve(app).await;
        let error = V1Sdk::new(&base, "").get_session("test").await.unwrap_err();
        assert!(
            matches!(&error, SdkError::Api { code, message } if code == "503" && message.contains("503 Service Unavailable")),
            "{error}"
        );
        let error = Client::new(&base).list_projects().await.unwrap_err();
        assert!(
            error.to_string().contains("503 Service Unavailable"),
            "{error}"
        );
        let error = V1Sdk::new(&base, "")
            .stream_session("test", 0)
            .await
            .err()
            .expect("stream must reject an HTTP error");
        assert!(
            matches!(error, SdkError::Http(error) if error.status() == Some(StatusCode::SERVICE_UNAVAILABLE))
        );
        server.abort();
    }
}
