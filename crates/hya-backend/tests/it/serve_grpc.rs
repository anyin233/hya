//! `hya serve` answers HTTP/1.1 and h2c gRPC on its readiness URL, shares
//! state with `HYA_GRPC_BIND`, and ends gRPC streams with `serverStopping` on SIGTERM.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use hya_api::v2 as pb;

use crate::common::Scratch;

struct Foreground {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Foreground {
    fn start(scratch: &Scratch, db: &std::path::Path, grpc_bind: &str) -> Self {
        let mut child = scratch
            .hya()
            .args(["serve", "--bind", "127.0.0.1:0", "--db"])
            .arg(db)
            .env("HYA_GRPC_BIND", grpc_bind)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
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
    let scratch = Scratch::new("hya-serve-grpc").unwrap();
    let db = scratch.0.join("s.db");
    let mut server = Foreground::start(&scratch, &db, "127.0.0.1:0");
    let url = server.wait_line("hya server listening on ");
    let extra = server.wait_line("hya grpc listening on ");
    assert_ne!(url, extra);

    let http = reqwest::Client::new();
    let health: serde_json::Value = http
        .get(format!("{url}/v2/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["ok"], serde_json::json!(true));

    let main = channel(&url).await;
    assert!(
        pb::process_client::ProcessClient::new(main.clone())
            .get_health(pb::GetHealthRequest::default())
            .await
            .unwrap()
            .into_inner()
            .ok
    );

    let pty: serde_json::Value = http
        .post(format!("{url}/v2/pty"))
        .json(&serde_json::json!({"shell": "/bin/sh", "cwd": scratch.0.to_string_lossy()}))
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

    let mut stream = pb::events_client::EventsClient::new(main)
        .stream_global_events(pb::StreamGlobalEventsRequest::default())
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // SAFETY: `kill` targets our unreaped child process.
    unsafe { libc::kill(i32::try_from(server.child.id()).unwrap(), libc::SIGTERM) };
    let mut last = None;
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(frame) = stream.next().await {
            let Ok(frame) = frame else { break };
            if let Some(pb::stream_frame::Frame::Event(event)) = frame.frame {
                last = event.payload;
            }
        }
    })
    .await
    .expect("gRPC stream ends on shutdown");
    match last {
        Some(pb::stream_event::Payload::ServerStopping(s)) => assert_eq!(s.reason, "signal"),
        other => panic!("last payload {other:?}"),
    }
    assert!(server.child.wait().unwrap().success());
}
