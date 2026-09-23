//! T2.26 — the packaged `hya-extra/zvec-grep` Plugin bundle installs, spawns
//! its stdio MCP server through a fake `zg` on `PATH`, and a full-plane agent
//! (`build`) calls the resulting namespaced tool and sees the server's result.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use hya_bundle::{BundleSource, write_public_package};
use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::json;

/// A minimal fake `zg` MCP stdio bridge: implements `initialize`, `tools/list`,
/// and `tools/call` for exactly one tool, `zvec_grep_search`, echoing back the
/// `root`/`query` arguments it received so the test can assert the real
/// bundled MCP server (not a builtin) answered the call.
const FAKE_ZG_SCRIPT: &str = r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    if "id" not in req:
        continue
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "zvec-grep-fake", "version": "0.0.1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": "zvec_grep_search",
                    "description": "Fake semantic search",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "root": {"type": "string"},
                            "query": {"type": "string"},
                        },
                        "required": ["root"],
                    },
                }
            ]
        }
    elif method == "tools/call":
        args = (req.get("params") or {}).get("arguments") or {}
        root = args.get("root", "")
        query = args.get("query", "")
        text = f"ZG_FAKE_RESULT root={root} query={query}"
        result = {"content": [{"type": "text", "text": text}], "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#;

/// Absolute path to the in-tree `bundles/extra/<name>` source directory.
fn extra_bundle_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/extra")
        .join(name)
        .canonicalize()
        .expect("extra bundle source directory exists")
}

/// Write an executable fake `zg` into a fresh directory and return that
/// directory (to be prepended to the backend's `PATH`).
fn install_fake_zg(root: &Path) -> PathBuf {
    let bin_dir = root.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let zg_path = bin_dir.join("zg");
    std::fs::write(&zg_path, FAKE_ZG_SCRIPT).expect("write fake zg");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&zg_path)
            .expect("stat fake zg")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&zg_path, perms).expect("chmod fake zg");
    }
    bin_dir
}

#[tokio::test]
async fn t2_26_zvec_grep_plugin_bundle_mcp_tool_executes_through_fake_zg() {
    let root = std::env::temp_dir().join(format!("hya-extra-zvec-grep-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let fake_bin_dir = install_fake_zg(&root);
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let source = BundleSource::read_directory(extra_bundle_dir("zvec-grep")).expect("read source");
    let package = root.join("zvec-grep.hyabundle");
    std::fs::write(&package, write_public_package(&source).expect("package")).unwrap();

    let env = E2eEnvBuilder::new()
        .backend_env("PATH", path)
        .scripts(vec![
            tool_step(
                "zvec-grep__mcp__zvec-grep__zvec_grep_search",
                json!({"root": "/workspace/marker", "query": "EXTRA_BUNDLE_QUERY_MARKER"}),
            ),
            text_step("ZVEC_GREP_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(&package).unwrap();

    let session = env.create_session().await.expect("root session");
    env.prompt(
        session,
        "search the workspace for EXTRA_BUNDLE_QUERY_MARKER",
    )
    .await
    .expect("prompt");

    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 1);
    assert!(
        followup.contains("ZG_FAKE_RESULT"),
        "{followup}; {}",
        env.diagnostics()
    );
    assert!(followup.contains("root=/workspace/marker"), "{followup}");
    assert!(
        followup.contains("query=EXTRA_BUNDLE_QUERY_MARKER"),
        "{followup}"
    );

    std::fs::remove_dir_all(&root).unwrap();
}

/// Marker substring of `build`'s engine-constructed system prompt (matches the
/// convention used by `p28_subagent_bundle.rs`).
const ROOT: &str = "You are hya";
/// Marker substring of `hya-extra/scout`'s own prompt (`prompts/scout.md`).
const SCOUT: &str = "You are scout, a cheap retrieval subagent";

#[tokio::test]
async fn t2_26_scout_subagent_bundle_spawns_and_calls_its_own_mcp_server() {
    let root = std::env::temp_dir().join(format!("hya-extra-scout-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let fake_bin_dir = install_fake_zg(&root);
    let path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let source = BundleSource::read_directory(extra_bundle_dir("scout")).expect("read source");
    let package = root.join("scout.hyabundle");
    std::fs::write(&package, write_public_package(&source).expect("package")).unwrap();

    let env = E2eEnvBuilder::new()
        .backend_env("PATH", path)
        .route(
            SCOUT,
            vec![
                tool_step(
                    "zvec-grep__zvec_grep_search",
                    json!({"root": "/workspace/marker", "query": "SCOUT_QUERY_MARKER"}),
                ),
                tool_step(
                    "report",
                    json!({
                        "result": "SCOUT_REPORT_OK src/marker.rs:1 ZG_FAKE_RESULT",
                        "outcome": "done"
                    }),
                ),
            ],
        )
        .route(
            ROOT,
            vec![
                tool_step(
                    "task",
                    json!({"members": [{
                        "description": "locate the marker",
                        "prompt": "find where the marker lives",
                        "subagent_type": "scout"
                    }]}),
                ),
                text_step("ROOT_SPAWNED_SCOUT"),
                text_step("ROOT_RECEIVED_SCOUT_RESULT"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(&package).unwrap();

    let session = env.create_session().await.expect("root session");
    env.prompt(session, "spawn scout to find the marker")
        .await
        .expect("prompt");

    let timeout = std::time::Duration::from_secs(20);
    env.wait_route_contains(ROOT, "SCOUT_REPORT_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("scout report missing: {error}; {}", env.diagnostics()));

    let scout_requests = env
        .fake
        .route_requests(SCOUT)
        .expect("route requests")
        .unwrap_or_default();
    assert!(
        scout_requests.len() >= 2,
        "expected a follow-up request to scout after its MCP tool call: {scout_requests:?}"
    );
    // The follow-up request carries the *result* of the MCP tool call (the
    // fake zg server's canned text), proving scout's own bundle-local
    // `zvec-grep` MCP resource actually executed — not just that the tool
    // call was scripted.
    let scout_followup = fake_requests_from(&scout_requests, 1);
    assert!(
        scout_followup.contains("ZG_FAKE_RESULT"),
        "{scout_followup}"
    );
    assert!(
        scout_followup.contains("root=/workspace/marker"),
        "{scout_followup}"
    );
    assert!(
        scout_followup.contains("query=SCOUT_QUERY_MARKER"),
        "{scout_followup}"
    );

    std::fs::remove_dir_all(&root).unwrap();
}
