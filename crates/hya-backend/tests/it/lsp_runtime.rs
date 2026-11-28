//! A configured stdio LSP server must be reachable through coding-extras'
//! shipped BundleApi, and the route must be absent while the bundle is off.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::common::{Scratch, TestResult, hya_bin};

/// Write `main.ts` and the fixture LSP config under `scratch`, start
/// `hya serve` with `config_yaml`, and request workspace symbols for `qaSymbol`.
async fn symbols_response(
    scratch: &Scratch,
    config_yaml: &str,
) -> Result<(tokio::process::Child, reqwest::Response), Box<dyn std::error::Error>> {
    let project = scratch.0.join("project");
    let config = scratch.0.join("config");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(config.join("hya"))?;
    std::fs::write(
        project.join("main.ts"),
        "export function qaSymbol() { return 42; }\n",
    )?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp_server.py");
    std::fs::write(config.join("hya/config.yaml"), config_yaml)?;
    let coding_extras_dir = config.join("hya/bundles/hya%2Fcoding-extras");
    std::fs::create_dir_all(&coding_extras_dir)?;
    std::fs::write(
        coding_extras_dir.join("config.yml"),
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
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(format!(
            "{url}/v2/bundles/hya%2Fcoding-extras/api/lsp/symbols"
        ))
        .query(&[
            ("query", "qaSymbol"),
            ("directory", project.to_string_lossy().as_ref()),
        ])
        .send()
        .await?;
    Ok((backend, response))
}

async fn stop(mut backend: tokio::process::Child) -> TestResult {
    backend.kill().await?;
    backend.wait().await?;
    Ok(())
}

#[tokio::test]
async fn configured_language_server_exposes_workspace_symbols() -> TestResult {
    let scratch = Scratch::new("hya-lsp-runtime")?;
    let (backend, response) = symbols_response(&scratch, "schema_version: 2\n").await?;
    let symbols: Value = response.error_for_status()?.json().await?;
    assert_eq!(
        symbols,
        json!({ "symbols": [{
            "path": "main.ts",
            "name": "qaSymbol",
            "kind": "SYMBOL_KIND_FUNCTION",
            "lineNumber": 1
        }]}),
        "configured LSP result missing"
    );
    stop(backend).await
}

#[tokio::test]
async fn disabled_coding_extras_has_no_symbols_route() -> TestResult {
    let scratch = Scratch::new("hya-lsp-runtime-disabled")?;
    let (backend, response) = symbols_response(
        &scratch,
        "schema_version: 2\nbundles:\n  builtin:\n    disable: [hya/coding-extras]\n",
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["code"], "bundle_api_not_found", "{body}");
    stop(backend).await
}
