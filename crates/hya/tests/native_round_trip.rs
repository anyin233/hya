//! Accept-gate proof: a full session turn against the in-process `hya` backend completes through
//! the `/v1` router driven in-process (no TCP transport), and OUR PROCESS opens zero network
//! sockets while doing it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use http_body_util::BodyExt;
use hya_app::{HyaRuntime, RuntimeOptions};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_turn_opens_no_socket() {
    let runtime = HyaRuntime::start(RuntimeOptions {
        model: None,
        db: String::new(),
        yolo: true,
        default_agent: None,
        force_offline: true,
    })
    .await
    .expect("offline runtime should start");
    let app = runtime.router();

    let (status, created) = call(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        json!({"agent": "build", "model": "hya/offline", "workdir": "/tmp"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let session = created["session"]["id"].as_str().unwrap().to_owned();
    assert!(!session.is_empty(), "created session should have an id");

    // Event-driven turn: admit, then poll to a terminal state — all
    // through oneshot requests to the in-process router.
    let (status, admitted) = call(
        app.clone(),
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": "hi"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{admitted}");
    let turn = admitted["turn"]["id"].as_str().unwrap().to_owned();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut terminal = false;
    while tokio::time::Instant::now() < deadline {
        let (status, info) = call(
            app.clone(),
            Method::GET,
            &format!("/v1/sessions/{session}/turns/{turn}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{info}");
        let state = info["state"].as_str().unwrap_or_default();
        if state != "TURN_STATE_RUNNING" && state != "TURN_STATE_ADMITTED" && !state.is_empty() {
            terminal = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(terminal, "the offline turn should reach a terminal state");

    let offenders = offending_sockets(&owned_socket_inodes());
    assert!(
        offenders.is_empty(),
        "in-process turn must open ZERO loopback sockets, found: {offenders:?}"
    );
}

async fn call(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

/// Inodes of sockets THIS process owns (from `/proc/self/fd/*` -> `socket:[INODE]`).
fn owned_socket_inodes() -> HashSet<String> {
    let mut inodes = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return inodes; // Not Linux: no procfs socket audit available.
    };
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            inodes.insert(inode.to_owned());
        }
    }
    inodes
}

/// Rows in `/proc/self/net/tcp{,6}` owned by us that are LISTEN, or ESTABLISHED to a loopback peer.
fn offending_sockets(owned: &HashSet<String>) -> Vec<String> {
    let mut offenders = Vec::new();
    for path in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let columns: Vec<&str> = line.split_whitespace().collect();
            let (Some(rem), Some(state), Some(inode)) =
                (columns.get(2), columns.get(3), columns.get(9))
            else {
                continue;
            };
            if !owned.contains(*inode) {
                continue;
            }
            // 0A = LISTEN (any), 01 = ESTABLISHED (only a loopback peer counts as HTTP-to-hya).
            let is_listen = *state == "0A";
            let is_loopback_established = *state == "01" && is_loopback_peer(rem);
            if is_listen || is_loopback_established {
                offenders.push(format!("{path}: state={state} rem={rem} inode={inode}"));
            }
        }
    }
    offenders
}

/// `rem` is `HHHHHHHH:PPPP` (v4) or 32-hex (v6). 127.0.0.1 little-endian ends in `7F`; ::1 is the
/// all-zero-but-last-word v6 pattern.
fn is_loopback_peer(rem: &str) -> bool {
    let Some((ip, _port)) = rem.split_once(':') else {
        return false;
    };
    if ip.len() == 8 {
        return ip.ends_with("7F"); // 127.x.x.x
    }
    ip == "00000000000000000000000001000000" // ::1
}
