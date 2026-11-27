//! A configured stdio LSP server must be reachable through the shipped HTTP surface.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::common::{Scratch, TestResult, hya_bin};

#[tokio::test]
async fn configured_language_server_exposes_workspace_symbols() -> TestResult {
    let scratch = Scratch::new("hya-lsp-runtime")?;
    let project = scratch.0.join("project");
    let config = scratch.0.join("config");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(config.join("hya"))?;
    std::fs::write(
        project.join("main.ts"),
        "export function qaSymbol() { return 42; }\n",
    )?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_server.py");
    std::fs::write(
        config.join("hya/config.yaml"),
        serde_norway::to_string(&json!({
            "lsp": {
                "typescript": {"disabled": true},
                "fixture": {"command": ["python3", fixture], "extensions": [".ts"]}
            }
        }))?,
    )?;
    let mut backend = tokio::process::Command::new(hya_bin())
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env("HOME", scratch.0.join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", scratch.0.join("state"))
        .env("XDG_DATA_HOME", scratch.0.join("data"))
        .env("XDG_CACHE_HOME", scratch.0.join("cache"))
        .current_dir(&project)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(backend.stdout.take().ok_or("no stdout")?).lines();
    let url = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                return Ok::<_, std::io::Error>(url.to_string());
            }
        }
        Err(std::io::Error::other("backend exited before readiness"))
    })
    .await??;
    let symbols: Value = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(format!("{url}/v1/fs/symbols"))
        .query(&[
            ("query", "qaSymbol"),
            ("directory", project.to_string_lossy().as_ref()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        symbols["symbols"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "qaSymbol")),
        "configured LSP result missing: {symbols}"
    );
    backend.kill().await?;
    backend.wait().await?;
    Ok(())
}
