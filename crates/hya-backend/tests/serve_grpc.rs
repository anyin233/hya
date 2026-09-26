//! `hya serve` answers gRPC on its HTTP port (docs/protocol/README.md
//! "gRPC"): the readiness URL speaks HTTP/1.1 and h2c gRPC alike, from one
//! server state. `HYA_GRPC_BIND` adds an optional extra listener serving
//! the same state, and a SIGTERM ends gRPC streams with `serverStopping`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt as _;
use hya_api::v1 as pb;

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A foreground `hya serve` whose stdout lines are collected; killed on
/// drop.
struct Foreground {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Foreground {
    fn start(root: &Path, db: &Path, grpc_bind: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("NO_COLOR", "1")
            .current_dir(root)
            .args(["serve", "--bind", "127.0.0.1:0", "--db"])
            .arg(db)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(bind) = grpc_bind {
            command.env("HYA_GRPC_BIND", bind);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Self { child, lines }
    }

    fn wait_line(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no `{prefix}` line"));
            if let Some(rest) = line.strip_prefix(prefix) {
                return rest.trim().to_owned();
            }
        }
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn channel(url: &str) -> tonic::transport::Channel {
    tonic::transport::Channel::from_shared(url.to_owned())
        .unwrap()
        .connect()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_readiness_url_answers_http_and_grpc_and_the_extra_listener_shares_the_state() {
    let root = scratch("hya-serve-grpc");
    let db = root.join("s.db");
    let mut server = Foreground::start(&root, &db, Some("127.0.0.1:0"));
    let url = server.wait_line("hya server listening on ");
    let extra = server.wait_line("hya grpc listening on ");
    assert_ne!(url, extra);

    // HTTP/1.1 and gRPC (h2c) on the one readiness URL.
    let http = reqwest::Client::new();
    let health: serde_json::Value = http
        .get(format!("{url}/v1/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["ok"], serde_json::json!(true), "{health}");
    let main = channel(&url).await;
    let health = pb::process_client::ProcessClient::new(main.clone())
        .get_health(pb::GetHealthRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(health.ok);

    // A PTY created over HTTP on the main port is visible over gRPC on the
    // extra listener at once: one server state.
    let pty: serde_json::Value = http
        .post(format!("{url}/v1/pty"))
        .json(&serde_json::json!({"shell": "/bin/sh", "cwd": root.to_string_lossy()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = pty["id"].as_str().expect("pty id").to_owned();
    let got = pb::pty_client::PtyClient::new(channel(&extra).await)
        .get_pty(pb::GetPtyRequest { id: id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.id, id);

    // A SIGTERM ends a gRPC stream on the main port with `serverStopping`.
    let mut stream = pb::events_client::EventsClient::new(main)
        .stream_global_events(pb::StreamGlobalEventsRequest::default())
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(i32::try_from(server.child.id()).unwrap(), libc::SIGTERM);
    }
    let mut last = None;
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(frame) = stream.next().await {
            let Ok(frame) = frame else { break };
            if let Some(pb::stream_frame::Frame::Event(event)) = frame.frame {
                last = event.payload;
            }
        }
    })
    .await
    .expect("the gRPC stream ends on shutdown");
    match last {
        Some(pb::stream_event::Payload::ServerStopping(stopping)) => {
            assert_eq!(stopping.reason, "signal");
        }
        other => panic!("last payload {other:?}"),
    }
    let status = server.child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    let _ = std::fs::remove_dir_all(&root);
}
