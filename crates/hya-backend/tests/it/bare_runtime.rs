//! Integration tests for unified global `--bare` runtime across `exec`, `serve`,
//! daemon lifecycle, `--bare --pure`, and zero-bundle bootability when
//! `bundles/builtin/` is completely absent (`HYA_FIRST_PARTY_SOURCE_ROOT` empty).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::header;
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::common::{
    Daemons, ForegroundServer, Scratch, TestResult, assert_success, combined_text, run_bounded,
};

struct RecordingProvider {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RecordingProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RecordingProvider {
    async fn start(reply_text: &'static str) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let recorded = Arc::clone(&recorded);
                async move {
                    if let Ok(mut guard) = recorded.lock() {
                        guard.push(body);
                    }
                    let chunk1 = json!({
                        "id": "chatcmpl-bare",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "model",
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": reply_text },
                            "finish_reason": Value::Null
                        }]
                    });
                    let chunk2 = json!({
                        "id": "chatcmpl-bare",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": "model",
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "stop"
                        }],
                        "usage": {
                            "prompt_tokens": 12,
                            "completion_tokens": 4,
                            "total_tokens": 16
                        }
                    });
                    let sse = format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n\n");
                    ([(header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
                }
            }),
        );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            base_url,
            requests,
            task,
        })
    }

    fn take_requests(&self) -> Vec<Value> {
        self.requests
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

fn setup_empty_builtin_root(scratch: &Scratch) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let empty_bundles = scratch.0.join("empty-first-party");
    fs::create_dir_all(&empty_bundles)?;
    Ok(empty_bundles)
}

fn write_fake_provider_config(
    scratch: &Scratch,
    base_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cfg_dir = scratch.0.join("config/hya");
    let auth_dir = cfg_dir.join("auth");
    fs::create_dir_all(&auth_dir)?;
    let yaml = format!(
        r#"schema_version: 2
default_model: fake/model
providers:
  fake:
    kind: openai-compatible
    base_url: {base_url}
    api_key: test-key
    models:
      - id: model
permission:
  model: allow
  rules: []
"#
    );
    fs::write(cfg_dir.join("config.yaml"), yaml)?;
    fs::write(auth_dir.join("fake.yaml"), "token: test-key\n")?;
    Ok(())
}

fn write_workspace_context(work: &Path, agents_marker: &str, skill_name: &str) -> TestResult {
    fs::create_dir_all(work.join(format!(".hya/skills/{skill_name}")))?;
    fs::write(
        work.join("AGENTS.md"),
        format!("# Rules\n{agents_marker}\n"),
    )?;
    fs::write(
        work.join(format!(".hya/skills/{skill_name}/SKILL.md")),
        format!(
            "---\nname: {skill_name}\ndescription: Bare workspace test skill.\n---\nSkill body.\n"
        ),
    )?;
    Ok(())
}

fn offered_tool_names(request: &Value) -> BTreeSet<String> {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn system_prompt_text(request: &Value) -> String {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|msg| msg["role"] == "system")
        .filter_map(|msg| msg["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_bare_boots_without_builtin_bundles_and_exposes_standard_bare_surface() -> TestResult {
    let scratch = Scratch::new("hya-bare-exec")?;
    let empty_bundles = setup_empty_builtin_root(&scratch)?;
    let work = scratch.0.join("work");
    write_workspace_context(&work, "BARE_AGENTS_MD_MARKER", "bare-local-skill")?;

    let provider = RecordingProvider::start("BARE_EXEC_REPLY").await?;
    write_fake_provider_config(&scratch, &provider.base_url)?;

    let mut cmd = scratch.hya();
    cmd.current_dir(&work)
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["--bare", "--yolo", "exec", "--json", "hello bare"]);
    let output = run_bounded(&mut cmd, Duration::from_secs(30))?;
    assert_success("hya --bare exec --json", &output);
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("BARE_EXEC_REPLY"), "{stdout}");

    // Also verify `--bare` placed after `exec` works identically.
    let mut after_cmd = scratch.hya();
    after_cmd
        .current_dir(&work)
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["--yolo", "exec", "--bare", "hello bare after"]);
    let after_out = run_bounded(&mut after_cmd, Duration::from_secs(30))?;
    assert_success("hya exec --bare", &after_out);
    assert!(
        String::from_utf8_lossy(&after_out.stdout).contains("BARE_EXEC_REPLY"),
        "{}",
        combined_text(&after_out)
    );

    let requests = provider.take_requests();
    assert_eq!(requests.len(), 2, "expected two exec model requests");
    let expected_tools: BTreeSet<String> = [
        "bash", "edit", "grep", "read", "rewrite", "skill", "todo", "write",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for request in &requests {
        assert_eq!(
            offered_tool_names(request),
            expected_tools,
            "bare mode must offer exactly the 8 standard bare tools"
        );
        let system = system_prompt_text(request);
        assert!(
            system.contains("BARE_AGENTS_MD_MARKER"),
            "bare mode must discover AGENTS.md: {system}"
        );
        assert!(
            system.contains("bare-local-skill"),
            "bare mode must discover local filesystem skills: {system}"
        );
        assert!(
            !system.contains("LINE#ID"),
            "bare mode must not inject hashline prompt instructions: {system}"
        );
    }

    // Bare non-TTY banner and `hya --bare agent list` also work with zero builtin bundles.
    let banner = scratch
        .hya()
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .arg("--bare")
        .output()?;
    assert_success("hya --bare banner", &banner);
    assert!(String::from_utf8(banner.stdout)?.contains("a multi-agent coding agent"));

    let agents = scratch
        .hya()
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["--bare", "agent", "list"])
        .output()?;
    assert_success("hya --bare agent list", &agents);
    let agents_out = String::from_utf8(agents.stdout)?;
    assert!(agents_out.contains("hya-bare"), "{agents_out}");
    assert!(!agents_out.contains("hya-main"), "{agents_out}");

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_bare_pure_skips_external_context_and_skills_without_builtin_bundles() -> TestResult {
    let scratch = Scratch::new("hya-bare-pure-exec")?;
    let empty_bundles = setup_empty_builtin_root(&scratch)?;
    let work = scratch.0.join("work");
    write_workspace_context(&work, "PURE_MUST_NOT_SEE_AGENTS", "pure-hidden-skill")?;

    let provider = RecordingProvider::start("PURE_BARE_REPLY").await?;
    write_fake_provider_config(&scratch, &provider.base_url)?;

    let mut cmd = scratch.hya();
    cmd.current_dir(&work)
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["exec", "--bare", "--pure", "--yolo", "benchmark turn"]);
    let output = run_bounded(&mut cmd, Duration::from_secs(30))?;
    assert_success("hya exec --bare --pure", &output);
    assert!(String::from_utf8(output.stdout)?.contains("PURE_BARE_REPLY"));

    let requests = provider.take_requests();
    assert_eq!(requests.len(), 1);
    let expected_tools: BTreeSet<String> = [
        "bash", "edit", "grep", "read", "rewrite", "skill", "todo", "write",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(offered_tool_names(&requests[0]), expected_tools);
    let system = system_prompt_text(&requests[0]);
    assert!(
        !system.contains("PURE_MUST_NOT_SEE_AGENTS"),
        "--bare --pure must skip AGENTS.md: {system}"
    );
    assert!(
        !system.contains("pure-hidden-skill"),
        "--bare --pure must skip external disk skills: {system}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_bare_and_daemon_boot_without_builtin_bundles() -> TestResult {
    let scratch = Scratch::new("hya-bare-serve")?;
    let empty_bundles = setup_empty_builtin_root(&scratch)?;
    let work = scratch.0.join("work");
    fs::create_dir_all(&work)?;
    let db = scratch.0.join("bare-serve.db");

    // 1. Foreground `hya serve --bare` with zero builtin bundles.
    let mut serve_cmd = scratch.hya();
    serve_cmd
        .current_dir(&work)
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["serve", "--bare", "--bind", "127.0.0.1:0", "--db"])
        .arg(&db);
    let disc = PathBuf::from(format!("{}.server.json", db.display()));
    let mut server = ForegroundServer::start(serve_cmd, "hya server listening on ", Some(&disc))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;

    let health: Value = client
        .get(format!("{}/v2/health", server.url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(health["ok"], Value::Bool(true));

    let agents: Value = client
        .get(format!("{}/v2/agents", server.url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let agent_ids: Vec<&str> = agents["agents"]
        .as_array()
        .ok_or("missing agents array")?
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        agent_ids.contains(&"hya-bare"),
        "expected hya-bare in /v2/agents: {agents}"
    );
    assert!(
        !agent_ids.contains(&"hya-main"),
        "hya-main must not appear in bare /v2/agents: {agents}"
    );

    let tools: Value = client
        .get(format!("{}/v2/tools", server.url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tool_names: BTreeSet<&str> = tools["tools"]
        .as_array()
        .ok_or("missing tools array")?
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    for expected in ["read", "write", "edit", "bash", "grep", "skill"] {
        assert!(
            tool_names.contains(expected),
            "missing bare tool {expected} in /v2/tools: {tools}"
        );
    }
    for forbidden in ["task", "ls", "glob", "find", "lsp", "webfetch", "send"] {
        assert!(
            !tool_names.contains(forbidden),
            "unexpected full-mode tool {forbidden} in bare /v2/tools: {tools}"
        );
    }

    let bundles: Value = client
        .get(format!("{}/v2/bundles", server.url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let first_party_bundles: Vec<&Value> = bundles["bundles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["scope"] == "first_party")
        .collect();
    assert!(
        first_party_bundles.is_empty(),
        "bare serve must expose zero first-party bundles: {bundles}"
    );

    let tui_exts: Value = client
        .get(format!("{}/v2/tui-extensions", server.url))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        tui_exts["extensions"].as_array().is_none_or(Vec::is_empty),
        "bare serve must expose zero TUI extensions: {tui_exts}"
    );

    let commands_status = client
        .get(format!("{}/v2/commands", server.url))
        .send()
        .await?
        .status();
    assert!(
        commands_status.is_success(),
        "/v2/commands failed in bare serve: {commands_status}"
    );

    // Create a session and run a turn on the bare server (offline echo model).
    let created: Value = client
        .post(format!("{}/v2/sessions", server.url))
        .json(&json!({ "workdir": work.to_string_lossy() }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let session_id = created["session"]["id"]
        .as_str()
        .ok_or("missing session id")?;
    assert_eq!(created["session"]["agent"], "hya-bare", "{created}");
    let turn: Value = client
        .post(format!("{}/v2/sessions/{session_id}/turns", server.url))
        .json(&json!({ "prompt": { "text": "bare server ping" } }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let turn_id = turn["turn"]["id"].as_str().ok_or("missing turn id")?;
    let waited: Value = client
        .post(format!(
            "{}/v2/sessions/{session_id}/turns/{turn_id}/wait",
            server.url
        ))
        .json(&json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert_eq!(waited["state"], "TURN_STATE_FINISHED", "{waited}");

    // `exec --bare` cannot route onto an already-running server holding `--db` (exit 75).
    let blocked = scratch
        .hya()
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .arg("--db")
        .arg(&db)
        .args(["exec", "--bare", "cannot route"])
        .output()?;
    assert_eq!(
        blocked.status.code(),
        Some(75),
        "{}",
        combined_text(&blocked)
    );

    let status = server.terminate()?;
    assert!(status.success(), "bare serve SIGTERM exit: {status:?}");

    // 2. Detached daemon `hya serve start --bare` / `status` / `stop` with zero builtin bundles.
    let daemon_db = scratch.0.join("bare-daemon.db");
    let mut daemons = Daemons(Vec::new());
    let started = scratch
        .hya()
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["serve", "start", "--bare", "--json", "--db"])
        .arg(&daemon_db)
        .output()?;
    assert_success("hya serve start --bare", &started);
    let info: Value = serde_json::from_slice(&started.stdout)?;
    let pid = i32::try_from(info["pid"].as_i64().ok_or("no pid")?)?;
    daemons.0.push(pid);
    let daemon_url = info["url"].as_str().ok_or("no url")?;

    let daemon_agents: Value = client
        .get(format!("{daemon_url}/v2/agents"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(
        daemon_agents["agents"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["name"] == "hya-bare")
                && !rows.iter().any(|r| r["name"] == "hya-main")),
        "bare daemon must serve hya-bare only: {daemon_agents}"
    );

    let stopped = scratch
        .hya()
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["serve", "stop", "--db"])
        .arg(&daemon_db)
        .output()?;
    assert_success("hya serve stop", &stopped);
    Ok(())
}
