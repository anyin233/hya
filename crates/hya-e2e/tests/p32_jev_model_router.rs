//! T2.27 — the packaged `hya-extra/jev-model-router` Plugin bundle runs its
//! Bun `chat.params` process, asks a stub Jev endpoint for the difficulty tier,
//! rewrites the turn's model to that tier, keeps a session on the same model
//! without asking Jev again, and falls back to `default_tier` when Jev fails.
//!
//! Requires `bun` on `PATH` (like the other Bun-backed bundle scenarios in
//! `p27_bundle_process.rs` and `p29_claude_bundle.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use hya_bundle::{BundleSource, write_public_package};
use hya_e2e::{E2eEnv, E2eEnvBuilder, text_step};
use serde_json::{Value, json};

const JEV_KEY: &str = "e2e-jev-key";

/// Canned Jev behaviour plus every request it received.
#[derive(Default)]
struct JevStub {
    /// `None` answers 200 with the choice below; `Some(code)` fails with it.
    fail_status: Option<u16>,
    choice: String,
    requests: Vec<(Option<String>, Value)>,
}

type SharedStub = Arc<Mutex<JevStub>>;

async fn systemone(
    State(stub): State<SharedStub>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut stub = stub.lock().unwrap();
    let auth = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    stub.requests.push((auth, body));
    if let Some(code) = stub.fail_status {
        return (
            StatusCode::from_u16(code).unwrap(),
            Json(json!({"error": "stub failure"})),
        );
    }
    let choice = &stub.choice;
    (
        StatusCode::OK,
        Json(json!({
            "model": "jev-stub",
            "answers": {"difficulty": {
                "type": "choice",
                "choice": choice,
                "probabilities": {"easy": 0.1, "hard": 0.9},
                "confidence": 0.9
            }},
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })),
    )
}

async fn start_jev_stub(stub: SharedStub) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/systemone", post(systemone))
        .with_state(stub);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

fn router_source_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/extra/jev-model-router")
        .canonicalize()
        .expect("jev-model-router source directory exists")
}

/// Build an env serving `fake/model` plus the two tier models, write the
/// router's bundle config (user scope), and install the packaged bundle.
async fn router_env(jev: SocketAddr, turns: usize) -> E2eEnv {
    let bun = std::process::Command::new("bun").arg("--version").output();
    assert!(
        bun.is_ok_and(|output| output.status.success()),
        "bun is required on PATH for the jev-model-router process"
    );
    let env = E2eEnvBuilder::new()
        .additional_models(["tier-easy", "tier-hard"])
        .scripts(
            (0..turns)
                .map(|turn| text_step(format!("TURN_{turn}")))
                .collect(),
        )
        .build()
        .await
        .expect("e2e env");

    let config_dir = env
        .backend
        .xdg_config_home
        .join("hya/bundles/hya-extra%2Fjev-model-router");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.yml"),
        format!(
            r#"jev:
  endpoint: http://{jev}/v1/systemone
  api_key: {JEV_KEY}
  timeout_ms: 10000
route:
  default_tier: easy
  stickiness: chain
tiers:
  - name: easy
    model: fake/tier-easy
    criteria: Short questions and trivial edits
  - name: hard
    model: fake/tier-hard
    criteria: Cross-cutting design and subtle debugging
"#
        ),
    )
    .unwrap();

    let source = BundleSource::read_directory(router_source_dir()).expect("read source");
    let package = env.backend.project.join("jev-model-router.hyabundle");
    std::fs::write(&package, write_public_package(&source).expect("package")).unwrap();
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
    env
}

fn provider_models(env: &E2eEnv) -> Vec<String> {
    env.fake
        .requests()
        .unwrap()
        .iter()
        .map(|request| request["model"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn t2_27_jev_picks_the_tier_and_the_session_stays_on_it() {
    let stub: SharedStub = Arc::new(Mutex::new(JevStub {
        choice: "hard".into(),
        ..JevStub::default()
    }));
    let jev = start_jev_stub(stub.clone()).await;
    let env = router_env(jev, 3).await;

    let session = env.create_session().await.expect("session");
    let turn = env
        .prompt(session, "JEV_FIRST_PROMPT redesign the scheduler")
        .await
        .expect("first turn");
    assert!(
        turn.error_message.is_empty(),
        "{}; {}",
        turn.error_message,
        env.diagnostics()
    );
    assert_eq!(
        provider_models(&env),
        ["tier-hard"],
        "the first turn streams from the tier Jev picked; {}",
        env.diagnostics()
    );

    {
        let stub = stub.lock().unwrap();
        assert_eq!(stub.requests.len(), 1, "exactly one Jev call");
        let (auth, body) = &stub.requests[0];
        assert_eq!(auth.as_deref(), Some(format!("Bearer {JEV_KEY}").as_str()));
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["difficulty"]["type"], "choice");
        assert_eq!(
            body["questions"]["difficulty"]["criteria"]["hard"],
            "Cross-cutting design and subtle debugging"
        );
        assert_eq!(body["state"]["agent"], "build");
        assert!(
            body["state"]["latest_user_message"]
                .as_str()
                .unwrap_or_default()
                .contains("JEV_FIRST_PROMPT"),
            "{body}"
        );
    }

    // Same request chain: sticky model, no second Jev call — even though Jev
    // would now answer differently.
    stub.lock().unwrap().choice = "easy".into();
    env.prompt(session, "a trivial follow-up")
        .await
        .expect("second turn");
    assert_eq!(provider_models(&env), ["tier-hard", "tier-hard"]);
    assert_eq!(
        stub.lock().unwrap().requests.len(),
        1,
        "sticky: Jev not re-asked"
    );

    // A new chain gets its own decision.
    let other = env.create_session().await.expect("second session");
    env.prompt(other, "what is 2+2").await.expect("third turn");
    assert_eq!(
        provider_models(&env),
        ["tier-hard", "tier-hard", "tier-easy"]
    );
    assert_eq!(stub.lock().unwrap().requests.len(), 2);
}

#[tokio::test]
async fn t2_27_jev_failure_routes_to_default_tier() {
    let stub: SharedStub = Arc::new(Mutex::new(JevStub {
        fail_status: Some(500),
        choice: "hard".into(),
        ..JevStub::default()
    }));
    let jev = start_jev_stub(stub.clone()).await;
    let env = router_env(jev, 1).await;

    let session = env.create_session().await.expect("session");
    let turn = env.prompt(session, "Jev is down").await.expect("turn");
    assert!(
        turn.error_message.is_empty(),
        "{}; {}",
        turn.error_message,
        env.diagnostics()
    );
    assert_eq!(
        stub.lock().unwrap().requests.len(),
        1,
        "the router asked Jev"
    );
    assert_eq!(
        provider_models(&env),
        ["tier-easy"],
        "a failed Jev call falls back to default_tier; {}",
        env.diagnostics()
    );
}
