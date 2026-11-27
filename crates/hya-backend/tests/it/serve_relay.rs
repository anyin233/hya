//! `hya serve --relay` and `hya serve relay …` (ADR-0025): joins an in-process
//! relay, prints its link once, persists identity, and rejoins across restart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hya_relay::client::{ClientConfig, RelayClient};
use hya_relay::link::RelayLink;
use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::common::{Scratch, assert_success, wait_until};

fn run(s: &Scratch, db: &Path, args: &[&str]) -> Output {
    s.hya()
        .arg("serve")
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert_success("serve relay", out);
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).unwrap()
}

fn marked_link(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("hya relay link: "))
        .map(str::to_owned)
}

fn relay_state(s: &Scratch, db: &Path) -> serde_json::Value {
    json(&run(s, db, &["relay", "status", "--json"]))
}

async fn health_through(link: &str) -> Result<String, String> {
    let link: RelayLink = link.parse().map_err(|e| format!("{e}"))?;
    let client =
        RelayClient::from_link(&link, ClientConfig::default()).map_err(|e| e.to_string())?;
    let leg = client
        .open(link.room_id())
        .await
        .map_err(|e| e.to_string())?;
    let mut tunnel = tokio::time::timeout(
        Duration::from_secs(10),
        NoiseStream::initiate_link(leg, &link, TunnelConfig::default()),
    )
    .await
    .map_err(|_| "handshake timed out".to_owned())?
    .map_err(|e| e.to_string())?;
    tunnel
        .write_all(b"GET /v2/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .map_err(|e| e.to_string())?;
    let mut resp = Vec::new();
    tunnel
        .read_to_end(&mut resp)
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&resp).into_owned())
}

async fn start_relay() -> (String, tokio::sync::oneshot::Sender<()>) {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let (addr, serve) = RelayServer::bind(
        RelayServerConfig::new("127.0.0.1:0".parse().unwrap()),
        async move {
            let _ = stopped.await;
        },
    )
    .await
    .unwrap();
    tokio::spawn(serve);
    (format!("http://127.0.0.1:{}", addr.port()), stop)
}

struct Foreground {
    child: Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}
impl Foreground {
    fn start(s: &Scratch, db: &Path, args: &[&str]) -> Self {
        let mut child = s
            .hya()
            .arg("serve")
            .args(args)
            .arg("--db")
            .arg(db)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = mpsc::channel();
        for pipe in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        Self {
            child,
            lines,
            seen: Vec::new(),
        }
    }
    fn wait_line(&mut self, prefix: &str) -> String {
        if let Some(l) = self.seen.iter().find(|l| l.starts_with(prefix)) {
            return l.clone();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no `{prefix}` line; saw {:#?}", self.seen));
            self.seen.push(line.clone());
            if line.starts_with(prefix) {
                return line;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serve_relay_joins_prints_the_link_once_and_is_controlled_over_loopback() {
    let (relay, _stop) = start_relay().await;
    let s = Scratch::new("hya-serve-relay").unwrap();
    let db = s.0.join("s.db");
    let mut srv = Foreground::start(&s, &db, &["--bind", "127.0.0.1:0", "--relay", &relay]);
    srv.wait_line("hya server listening on ");
    let link = marked_link(&srv.wait_line("hya relay link: ")).unwrap();
    let secret = link.split_once('#').unwrap().1.to_owned();
    assert!(link.starts_with("hya+insecure://127.0.0.1:"), "{link}");

    wait_until("relay connected", Duration::from_secs(15), || {
        relay_state(&s, &db)["state"] == "RELAY_STATE_CONNECTED"
    })
    .unwrap();
    let human = stdout(&run(&s, &db, &["relay", "status"]));
    assert!(human.starts_with("relay connected") && !human.contains(&secret));
    assert_eq!(stdout(&run(&s, &db, &["relay", "link"])), link);

    let id = PathBuf::from(format!("{}.relay-identity.json", db.display()));
    assert_eq!(
        std::fs::metadata(&id).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let disc = std::fs::read_to_string(format!("{}.server.json", db.display())).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&disc).unwrap()["relay"]["proxyUrl"],
        relay
    );
    assert!(
        !disc.contains(&secret) && health_through(&link).await.unwrap().contains("\"ok\":true")
    );

    let new = marked_link(&stdout(&run(&s, &db, &["relay", "rotate"]))).unwrap();
    assert!(
        new != link
            && health_through(&link).await.is_err()
            && health_through(&new).await.unwrap().contains("\"ok\":true")
    );

    assert_eq!(
        stdout(&run(&s, &db, &["relay", "disconnect"])),
        "relay disconnected"
    );
    assert!(
        !std::fs::read_to_string(format!("{}.server.json", db.display()))
            .unwrap()
            .contains("relay")
    );
    assert_eq!(run(&s, &db, &["relay", "link"]).status.code(), Some(1));
    assert_eq!(
        json(&run(&s, &db, &["relay", "connect", &relay, "--json"]))["link"],
        new
    );
    wait_until("reconnected", Duration::from_secs(15), || {
        relay_state(&s, &db)["state"] == "RELAY_STATE_CONNECTED"
    })
    .unwrap();
    assert!(health_through(&new).await.unwrap().contains("\"ok\":true"));

    // SAFETY: `kill` targets our unreaped child process.
    unsafe { libc::kill(i32::try_from(srv.child.id()).unwrap(), libc::SIGTERM) };
    assert!(srv.child.wait().unwrap().success());
    while let Ok(line) = srv.lines.recv_timeout(Duration::from_millis(100)) {
        srv.seen.push(line);
    }
    assert_eq!(srv.seen.iter().filter(|l| l.contains(&secret)).count(), 1);
    assert_eq!(run(&s, &db, &["relay", "status"]).status.code(), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_daemon_rejoins_with_the_same_link_after_restart() {
    let (relay, _stop) = start_relay().await;
    let s = Scratch::new("hya-serve-relay-daemon").unwrap();
    let db = s.0.join("s.db");
    let started = run(&s, &db, &["start", "--json", "--relay", &relay]);
    let first_pid = json(&started)["pid"].as_i64().unwrap();
    let link = marked_link(&String::from_utf8_lossy(&started.stderr)).expect("start prints link");
    let secret = link.split_once('#').unwrap().1.to_owned();
    assert!(
        !std::fs::read_to_string(format!("{}.server.log", db.display()))
            .unwrap()
            .contains(&secret)
    );
    wait_until("daemon joined", Duration::from_secs(15), || {
        relay_state(&s, &db)["state"] == "RELAY_STATE_CONNECTED"
    })
    .unwrap();

    let ready = json(&run(&s, &db, &["restart", "--json"]));
    assert!(ready["restarted"] == true && ready["pid"].as_i64().unwrap() != first_pid);
    wait_until("restarted joined", Duration::from_secs(15), || {
        let out = run(&s, &db, &["relay", "status", "--json"]);
        out.status.success() && json(&out)["state"] == "RELAY_STATE_CONNECTED"
    })
    .unwrap();
    assert_eq!(stdout(&run(&s, &db, &["relay", "link"])), link);
    assert!(health_through(&link).await.unwrap().contains("\"ok\":true"));
    assert!(run(&s, &db, &["stop"]).status.success());
}
