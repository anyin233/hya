//! Smoke CLI wiring test for `hya relay doctor` exit codes and JSON output.
//! Target parsing, redaction, stdin/argv warnings, and report formatting live
//! in `src/relay_doctor.rs` unit tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

use hya_relay::server::{RelayServer, RelayServerConfig};
use serde_json::Value;
use tokio::net::TcpListener;

use crate::common::TestResult;

fn doctor(target: &str) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_hya"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("NO_COLOR", "1")
        .args(["relay", "doctor", target, "--timeout", "3", "--json"])
        .output()?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_doctor_cli_exits_0_when_reachable_and_1_when_unreachable() -> TestResult {
    let config = RelayServerConfig::new("127.0.0.1:0".parse().expect("valid addr"));
    let (addr, serve) = RelayServer::bind(config, std::future::pending()).await?;
    let relay = tokio::spawn(serve);

    let ok = doctor(&format!("http://{addr}"))?;
    relay.abort();
    assert!(ok.status.success(), "{ok:?}");
    let report: Value = serde_json::from_slice(&ok.stdout)?;
    assert_eq!(report["reachable"], true);
    assert_eq!(report["recommended_transport"], "auto");

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let closed = listener.local_addr()?;
    drop(listener);
    let down = doctor(&format!("http://{closed}"))?;
    assert_eq!(down.status.code(), Some(1), "{down:?}");
    let report: Value = serde_json::from_slice(&down.stdout)?;
    assert_eq!(report["reachable"], false);
    assert!(report["recommended_transport"].is_null());
    Ok(())
}
