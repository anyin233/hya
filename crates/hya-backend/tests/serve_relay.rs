//! `hya serve --relay` and `hya serve relay …` (ADR-0025; docs/relay.md
//! "Hosting a backend on a relay"): a backend joins an in-process relay,
//! prints its link once on stderr, keeps its identity in
//! `<db>.relay-identity.json`, records the relay (never the link) in the
//! discovery file, and is controlled over the loopback-only `RelayControl`
//! rpcs. A daemon started with `--relay` rejoins the same relay, with the
//! same link, after `hya serve restart`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hya_relay::client::{ClientConfig, RelayClient};
use hya_relay::link::RelayLink;
use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::tunnel::{NoiseStream, TunnelConfig};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// `hya serve <args> --db <db>` with an isolated HOME/XDG under `root`.
fn serve(root: &Path, db: &Path, args: &[&str]) -> Command {
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
        .arg("serve")
        .args(args)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::null());
    command
}

fn run(root: &Path, db: &Path, args: &[&str]) -> Output {
    serve(root, db, args).output().unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).unwrap()
}

/// The link after the `hya relay link:` marker in `text`.
fn marked_link(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("hya relay link: "))
        .map(str::to_owned)
}

fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn relay_state(root: &Path, db: &Path) -> serde_json::Value {
    json(&run(root, db, &["relay", "status", "--json"]))
}

/// `GET /v1/health` through the relay with `link`.
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
        .write_all(b"GET /v1/health HTTP/1.1\r\nHost: hya\r\nConnection: close\r\n\r\n")
        .await
        .map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    tunnel
        .read_to_end(&mut response)
        .await
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&response).into_owned())
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

/// A foreground `hya serve` whose stdout and stderr lines are collected.
struct Foreground {
    child: Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Foreground {
    fn start(root: &Path, db: &Path, args: &[&str]) -> Self {
        let mut child = serve(root, db, args)
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
        if let Some(line) = self.seen.iter().find(|line| line.starts_with(prefix)) {
            return line.clone();
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
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
    let root = scratch("hya-serve-relay");
    let db = root.join("s.db");
    let mut server = Foreground::start(&root, &db, &["--bind", "127.0.0.1:0", "--relay", &relay]);
    server.wait_line("hya server listening on ");
    let link = marked_link(&server.wait_line("hya relay link: ")).unwrap();
    assert!(link.starts_with("hya+insecure://127.0.0.1:"), "{link}");
    let secret = link.split_once('#').unwrap().1.to_owned();

    // Status: connected, and never the secret.
    wait_until("the relay is connected", || {
        relay_state(&root, &db)["state"] == "RELAY_STATE_CONNECTED"
    });
    let human = stdout(&run(&root, &db, &["relay", "status"]));
    assert!(human.starts_with("relay connected"), "{human}");
    assert!(!human.contains(&secret), "{human}");
    assert_eq!(stdout(&run(&root, &db, &["relay", "link"])), link);

    // The identity file is private; the discovery file names the relay only.
    let identity = PathBuf::from(format!("{}.relay-identity.json", db.display()));
    let mode = std::fs::metadata(&identity).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let discovery = std::fs::read_to_string(format!("{}.server.json", db.display())).unwrap();
    let value: serde_json::Value = serde_json::from_str(&discovery).unwrap();
    assert_eq!(value["relay"]["proxyUrl"], serde_json::json!(relay));
    assert!(!discovery.contains(&secret), "{discovery}");

    let response = health_through(&link).await.unwrap();
    assert!(response.contains("\"ok\":true"), "{response}");

    // Rotate: the old link stops working, the new one works.
    let rotated = run(&root, &db, &["relay", "rotate"]);
    let new = marked_link(&stdout(&rotated)).unwrap();
    assert_ne!(new, link);
    assert!(
        health_through(&link).await.is_err(),
        "the old link is revoked"
    );
    assert!(health_through(&new).await.unwrap().contains("\"ok\":true"));

    // Disconnect clears the discovery record; connect rejoins.
    let out = stdout(&run(&root, &db, &["relay", "disconnect"]));
    assert_eq!(out, "relay disconnected");
    let discovery = std::fs::read_to_string(format!("{}.server.json", db.display())).unwrap();
    assert!(!discovery.contains("relay"), "{discovery}");
    let failed = run(&root, &db, &["relay", "link"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("not connected"));
    let joined = run(&root, &db, &["relay", "connect", &relay, "--json"]);
    let joined = json(&joined);
    assert_eq!(joined["link"], serde_json::json!(new), "the same identity");
    wait_until("the relay is connected again", || {
        relay_state(&root, &db)["state"] == "RELAY_STATE_CONNECTED"
    });
    assert!(health_through(&new).await.unwrap().contains("\"ok\":true"));

    // The link was printed exactly once by the server.
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(i32::try_from(server.child.id()).unwrap(), libc::SIGTERM);
    }
    let status = server.child.wait().unwrap();
    assert!(status.success(), "{status:?}");
    while let Ok(line) = server.lines.recv_timeout(Duration::from_millis(500)) {
        server.seen.push(line);
    }
    let printed = server
        .seen
        .iter()
        .filter(|line| line.contains(&secret))
        .count();
    assert_eq!(printed, 1, "{:#?}", server.seen);
    // No server: `serve relay` fails with exit 1.
    let none = run(&root, &db, &["relay", "status"]);
    assert_eq!(none.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_daemon_rejoins_with_the_same_link_after_restart() {
    let (relay, _stop) = start_relay().await;
    let root = scratch("hya-serve-relay-daemon");
    let db = root.join("s.db");
    let started = run(&root, &db, &["start", "--json", "--relay", &relay]);
    let info = json(&started);
    let first_pid = info["pid"].as_i64().unwrap();
    let link =
        marked_link(&String::from_utf8_lossy(&started.stderr)).expect("start prints the link");
    let log = std::fs::read_to_string(format!("{}.server.log", db.display())).unwrap();
    let secret = link.split_once('#').unwrap().1.to_owned();
    assert!(
        !log.contains(&secret),
        "the daemon log never holds the link"
    );
    wait_until("the daemon joined the relay", || {
        relay_state(&root, &db)["state"] == "RELAY_STATE_CONNECTED"
    });

    let restarted = run(&root, &db, &["restart", "--json"]);
    let second = json(&restarted);
    assert_ne!(second["pid"].as_i64().unwrap(), first_pid);
    let again =
        marked_link(&String::from_utf8_lossy(&restarted.stderr)).expect("restart prints the link");
    assert_eq!(again, link, "same identity, same relay");
    wait_until("the new daemon joined the relay", || {
        relay_state(&root, &db)["state"] == "RELAY_STATE_CONNECTED"
    });
    let status = json(&run(&root, &db, &["status", "--json"]));
    assert_eq!(status["relay"]["proxyUrl"], serde_json::json!(relay));
    assert!(health_through(&link).await.unwrap().contains("\"ok\":true"));

    let stopped = run(&root, &db, &["stop"]);
    assert!(stopped.status.success());
    let _ = std::fs::remove_dir_all(&root);
}
