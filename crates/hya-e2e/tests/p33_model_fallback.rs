//! T2.28 — the packaged `hya-extra/model-fallback` Plugin bundle installs, its
//! Bun process consults `hook/model.fallback` when a session's configured
//! model fails before any stream exists, and the bundle's `config.yml` chain
//! decides whether the turn recovers on a fallback model or the original
//! provider error surfaces.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use hya_api::v1 as pb;
use hya_bundle::{BundleSource, write_public_package};
use hya_e2e::{E2eEnv, E2eEnvBuilder, text_step};
use hya_proto::SessionId;

/// Absolute path to the in-tree `bundles/extra/model-fallback` source directory.
fn model_fallback_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/extra/model-fallback")
        .canonicalize()
        .expect("model-fallback bundle source directory exists")
}

/// Package the real bundle and install it into `env`'s isolated data home.
fn install_model_fallback_bundle(env: &E2eEnv, root: &Path) {
    let source = BundleSource::read_directory(model_fallback_dir()).expect("read source");
    let package = root.join("model-fallback.hyabundle");
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
}

/// Write the bundle's user-scope `config.yml` at
/// `<hya config dir>/bundles/hya-extra%2Fmodel-fallback/config.yml`, per
/// docs/configuration.md#bundle-configuration-files.
fn write_model_fallback_config(env: &E2eEnv, body: &str) {
    let dir = env
        .backend
        .xdg_config_home
        .join("hya")
        .join("bundles")
        .join("hya-extra%2Fmodel-fallback");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yml"), body).unwrap();
}

/// Create a session pinned to an explicit (possibly unrouted) model, bypassing
/// [`hya_e2e::E2eEnv::create_session`]'s fixed `env.model`.
async fn create_session_with_model(env: &E2eEnv, model: &str) -> SessionId {
    let resp = env
        .client
        .create_session(&pb::CreateSessionRequest {
            agent: env.agent.clone(),
            model: model.to_string(),
            workdir: env.backend.workdir_str(),
            ..Default::default()
        })
        .await
        .expect("create session");
    resp.session
        .and_then(|session| session.id.parse().ok())
        .expect("create session response carries an id")
}

#[tokio::test]
async fn t2_28_model_fallback_plugin_recovers_an_unrouted_model_before_the_stream() {
    let root = std::env::temp_dir().join(format!(
        "hya-extra-model-fallback-e2e-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("RECOVERED_ON_FALLBACK")])
        .build()
        .await
        .expect("e2e env");

    install_model_fallback_bundle(&env, &root);
    // `ghost/unrouted` has no provider route (unknown_model, pre-stream); the
    // bundle's chain sends the round to `fake/model`, which FakeLlm serves.
    write_model_fallback_config(
        &env,
        r#"
chains:
  ghost/unrouted: [fake/model]
on: [retryable, unknown_model]
max_attempts: 3
"#,
    );

    let session = create_session_with_model(&env, "ghost/unrouted").await;
    let turn = env.prompt(session, "route me").await.unwrap();
    assert!(
        turn.error_message.is_empty(),
        "model.fallback must recover the unrouted model: {}; {}",
        turn.error_message,
        env.diagnostics()
    );
    assert_eq!(
        turn.state,
        pb::TurnState::Finished as i32,
        "expected the turn to finish once model.fallback recovered: {turn:?}; {}",
        env.diagnostics()
    );

    // No provider request was ever sent for `ghost/unrouted` (it fails before
    // a stream exists, since no route claims it); the only request FakeLlm
    // saw is the one recovery made on `fake/model`.
    let requests = env.fake.requests().unwrap();
    assert_eq!(requests.len(), 1, "{}", env.diagnostics());

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn t2_28_model_fallback_plugin_gives_up_and_surfaces_the_error_with_no_matching_chain() {
    let root = std::env::temp_dir().join(format!(
        "hya-extra-model-fallback-give-up-e2e-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("UNREACHABLE")])
        .build()
        .await
        .expect("e2e env");

    install_model_fallback_bundle(&env, &root);
    // `ghost/no-chain` has no entry in `chains` and `default` is empty, so the
    // bundle gives up on the very first consult: the original unknown_model
    // provider error must surface on the turn instead of a silent hang.
    write_model_fallback_config(
        &env,
        r#"
chains:
  ghost/unrouted: [fake/model]
default: []
on: [retryable, unknown_model]
max_attempts: 3
"#,
    );

    let session = create_session_with_model(&env, "ghost/no-chain").await;
    let turn = env.prompt(session, "route me").await.unwrap();
    assert_eq!(
        turn.state,
        pb::TurnState::Failed as i32,
        "an exhausted chain must surface the provider error, not recover silently: {turn:?}; {}",
        env.diagnostics()
    );

    let requests = env.fake.requests().unwrap();
    assert!(
        requests.is_empty(),
        "an unrouted model with no fallback never reaches the provider: {}",
        env.diagnostics()
    );

    std::fs::remove_dir_all(&root).unwrap();
}
