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
