//! Accept-gate proof: a full session turn against the in-process `hya` backend completes through
//! the native transport, and OUR PROCESS opens zero network sockets while doing it.

use std::collections::HashSet;
use std::time::Duration;

use hya_app::{HyaRuntime, RuntimeOptions};
use hya_hya::{spawn_event_bridge, HyaNativeTransport};
use hya_sdk::{ApiClient, Client, GlobalEvent};
use serde_json::json;
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_turn_opens_no_socket() {
    let runtime = HyaRuntime::start(RuntimeOptions {
        model: None,
        db: String::new(),
        yolo: true,
        default_agent: None,
        include_global_agents: false,
        force_offline: true,
    })
    .await
    .expect("offline runtime should start");

    let client =
        ApiClient::with_transport(HyaNativeTransport::new(runtime.router().clone(), "/tmp"));
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
    let _bridge = spawn_event_bridge(runtime.router().clone(), "/tmp".to_owned(), event_tx);

    let session = client.session_create().await.expect("session_create");
    assert!(!session.id.is_empty(), "created session should have an id");

    client
        .session_prompt(
            &session.id,
            json!({ "parts": [{ "type": "text", "text": "hi" }] }),
        )
        .await
        .expect("session_prompt should be admitted");

    let mut kinds = Vec::new();
    let mut saw_turn_event = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), event_rx.recv()).await {
            Ok(Some(event)) => {
                let kind = event.payload.kind.clone();
                if kind != "server.connected" && kind != "server.heartbeat" {
                    saw_turn_event = true;
                    kinds.push(kind);
                    break;
                }
                kinds.push(kind);
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(
        kinds.iter().any(|k| k == "server.connected"),
        "bridge should deliver server.connected; saw {kinds:?}"
    );
    assert!(
        saw_turn_event,
        "the offline turn should forward at least one non-connected event; saw {kinds:?}"
    );

    let offenders = offending_sockets(&owned_socket_inodes());
    assert!(
        offenders.is_empty(),
        "native turn must open ZERO inet sockets, found: {offenders:?}"
    );
}

/// Inodes of sockets THIS process owns (from `/proc/self/fd/*` -> `socket:[INODE]`).
fn owned_socket_inodes() -> HashSet<String> {
    let mut inodes = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return inodes;
    };
    for entry in entries.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let target = target.to_string_lossy();
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|rest| rest.strip_suffix(']'))
            {
                inodes.insert(inode.to_owned());
            }
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
