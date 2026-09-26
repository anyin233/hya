//! `hya relay doctor`: probes an in-process `RelayServer` (docs/relay.md
//! "Diagnostics", ADR-0025 D9). The relay runs in this test process; `hya
//! relay doctor` itself is spawned as a subprocess, like every other CLI
//! test in this crate (`sessions_cli.rs`, `proxy_cli.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::process::{Command, Output};

use hya_relay::server::{RelayServer, RelayServerConfig};
use hya_relay::testing::Http1OnlyHop;
use serde_json::Value;
use tokio::net::TcpListener;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn doctor(target: &str, extra: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_COLOR", "1")
        .args(["relay", "doctor", target, "--timeout", "3"])
        .args(extra);
    Ok(command.output()?)
}

/// Start a relay on an ephemeral loopback port; stopped when the returned
/// future is dropped (aborting the serve task is enough for a test).
async fn start_relay() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let config = RelayServerConfig::new("127.0.0.1:0".parse().expect("valid addr"));
    let (addr, serve) = RelayServer::bind(config, std::future::pending())
        .await
        .expect("relay binds");
    (addr, tokio::spawn(serve))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_ok_recommends_auto_and_reports_both_bindings() -> TestResult {
    let (addr, relay) = start_relay().await;
    let output = doctor(&format!("http://{addr}"), &["--json"])?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["reachable"], true);
    assert_eq!(report["grpc"]["ok"], true, "{report}");
    assert_eq!(report["ws"]["ok"], true, "{report}");
    assert_eq!(report["recommended_transport"], "auto");
    assert_eq!(report["prefix_ok"], true);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_blocked_by_an_http1_only_hop_recommends_ws() -> TestResult {
    let (addr, relay) = start_relay().await;
    let hop = Http1OnlyHop::start(addr).await?;
    let output = doctor(&format!("http://{}", hop.addr), &["--json"])?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["reachable"], true);
    assert_eq!(report["grpc"]["ok"], false, "{report}");
    assert_eq!(report["grpc"]["kind"], "NoHttp2", "{report}");
    assert_eq!(report["ws"]["ok"], true, "{report}");
    assert_eq!(report["recommended_transport"], "ws");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_reachable_exits_1_and_recommends_nothing() -> TestResult {
    // Bind, then drop the listener: the port refuses connections but is
    // valid and free of any relay.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);
    let output = doctor(&format!("http://{addr}"), &["--json"])?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["reachable"], false, "{report}");
    assert_eq!(report["grpc"]["ok"], false, "{report}");
    assert_eq!(report["ws"]["ok"], false, "{report}");
    assert!(report["recommended_transport"].is_null(), "{report}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_report_names_the_target_and_the_recommendation() -> TestResult {
    let (addr, relay) = start_relay().await;
    let output = doctor(&format!("http://{addr}"), &[])?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains(&addr.to_string()), "{text}");
    assert!(text.contains("recommended t=auto"), "{text}");
    Ok(())
}

const ROOM: &str = "eh7ddx5bksrgcytl7bkai36se4";
const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const PSK_B64: &str = "S3CR3TS3CR3TS3CR3TS3CR3TS3CR3TS3CR3TS3CR3TA";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_redacted_link_is_enough_and_no_warning_is_printed() -> TestResult {
    let (addr, relay) = start_relay().await;
    let target = format!("hya+insecure://{addr}/{ROOM}");
    let output = doctor(&target, &["--json"])?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["target"], target.as_str(), "{report}");
    assert_eq!(report["reachable"], true, "{report}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!stderr.contains("process listings"), "{stderr}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_link_argument_warns_about_argv_and_never_prints_the_secret() -> TestResult {
    let (addr, relay) = start_relay().await;
    let target = format!("hya+insecure://{addr}/{ROOM}#{KEY_B64}.{PSK_B64}");
    let output = doctor(&target, &[])?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("process listings"), "{stderr}");
    for text in [&stdout, &stderr] {
        assert!(!text.contains("S3CR3T"), "{text}");
    }
    assert!(
        stdout.contains(&format!("hya+insecure://{addr}/{ROOM}")),
        "{stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_link_on_stdin_is_read_without_the_argv_warning() -> TestResult {
    use std::io::Write as _;
    let (addr, relay) = start_relay().await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_hya"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_COLOR", "1")
        .args(["relay", "doctor", "-", "--timeout", "3", "--json"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(format!("hya+insecure://{addr}/{ROOM}#{KEY_B64}.{PSK_B64}\n").as_bytes())?;
    let output = child.wait_with_output()?;
    relay.abort();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["reachable"], true, "{report}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!stderr.contains("process listings"), "{stderr}");
    assert!(!stderr.contains("S3CR3T") && !report.to_string().contains("S3CR3T"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_target_error_never_echoes_its_fragment() -> TestResult {
    let output = doctor("https://relay.example.com/x#S3CR3TKEY.S3CR3TPSK", &[])?;
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!stderr.contains("S3CR3T"), "{stderr}");
    Ok(())
}
