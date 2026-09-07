//! A configured stdio server must be reachable through the shipped HTTP surface.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn configured_language_server_exposes_workspace_symbols()
-> Result<(), Box<dyn std::error::Error>> {
    let root = Workspace(std::env::temp_dir().join(format!(
        "hya-lsp-runtime-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let project = root.0.join("project");
    let config = root.0.join("config");
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
    let mut backend = tokio::process::Command::new(env!("CARGO_BIN_EXE_hya-backend"))
        .args(["serve", "--bind", "127.0.0.1:0"])
        .env("HOME", root.0.join("home"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", root.0.join("state"))
        .env("XDG_DATA_HOME", root.0.join("data"))
        .env("XDG_CACHE_HOME", root.0.join("cache"))
        .current_dir(&project)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = backend.stdout.take().ok_or("missing backend stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let url = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await? {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                return Ok::<_, std::io::Error>(url.to_string());
            }
        }
        Err(std::io::Error::other("backend exited before readiness"))
    })
    .await??;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let symbols: Value = client
        .get(format!("{url}/find/symbol"))
        .query(&[("query", "qaSymbol")])
        .header("x-opencode-directory", project.to_string_lossy().as_ref())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        symbols
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "qaSymbol")),
        "configured LSP result missing: {symbols}"
    );
    let status: Value = client
        .get(format!("{url}/lsp"))
        .header("x-opencode-directory", project.to_string_lossy().as_ref())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        status.as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == "fixture" && row["status"] == "connected")),
        "connected server missing from status: {status}"
    );
    backend.kill().await?;
    backend.wait().await?;
    Ok(())
}
