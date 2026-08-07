//! Integration tests for `hya-app`: installed bundle refresh.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};
use hya_core::{EventBus, RuntimeRegistry, SessionEngine};
use hya_provider::ProviderRouter;
use hya_store::{BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn temp_path(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch");
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-installed-bundle-refresh-{}-{}-{id}-{suffix}",
        elapsed.as_nanos(),
        std::process::id()
    ))
}

fn builtin_source() -> BundleSource {
    BundleSource::new(
        "builtin",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/builtin-test
  version: 1.0.0
  publisher: hya
agent:
  id: general
  role: main
  prompt: prompts/general.md
  spawn_lifecycle: transient
"#,
            ),
            SourceFile::new("prompts/general.md", b"You are general.\n".as_slice()),
        ],
    )
}

fn installed_source() -> BundleSource {
    BundleSource::new(
        "installed",
        vec![SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/installed-test
  version: 1.0.0
  publisher: hya
agent:
  id: installed-agent
  role: main
  spawn_lifecycle: transient
---
You are the installed agent.
"#,
        )],
    )
}

#[tokio::test]
async fn installed_generation_refresh_publishes_only_for_new_root_bindings() {
    let prepared_builtins = prepare_package(builtin_source()]).expect("prepare builtins");
    let builtins = prepared_builtins.bundles().to_vec();
    let catalog = Arc::new(
        BundleCatalog::from_verified_catalogs(&[&prepared_builtins])
            .expect("build builtin catalog"),
    );
    let runtime = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::clone(&catalog),
    ));
    let registry_path = temp_path("registry.db");
    let registry =
        BundleRegistry::connect(registry_path.to_str().expect("registry path must be UTF-8"))
            .await
            .expect("connect bundle registry");
    let refresh = Arc::new(hya_app::InstalledBundleRefresh::new(
        registry_path,
        Arc::clone(&catalog),
    ));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.expect("connect store"),
        Arc::new(ProviderRouter::new()),
        Arc::clone(&runtime),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    let workdir = temp_path("workdir");
    std::fs::create_dir_all(&workdir).expect("create test workdir");

    let old_binding = engine.bind_runtime(&workdir).expect("bind old catalog");
    let old_generation = old_binding.generation();
    assert!(old_binding.resolve_agent("general").is_some());
    assert!(old_binding.resolve_agent("installed-agent").is_none());

    let installed = prepare_package(installed_source()).expect("prepare installed bundle");
    let outcome = registry
        .install(
            &builtins,
            BundleInstallCandidate {
                source_digest: [0x42; 32],
                prepared_digest: installed.digest().to_owned(),
                prepared_bytes: installed.bytes().to_vec(),
                installed_at: 1_725_000_010,
            },
        )
        .await
        .expect("install bundle");
    assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

    let before_refresh = engine
        .bind_runtime(&workdir)
        .expect("bind before installed refresh");
    assert_eq!(before_refresh.generation(), old_generation);
    assert!(before_refresh.resolve_agent("installed-agent").is_none());

    let fresh_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind installed catalog");
    assert_eq!(fresh_binding.generation().get(), old_generation.get() + 1);
    assert!(fresh_binding.resolve_agent("installed-agent").is_some());
    assert!(old_binding.resolve_agent("installed-agent").is_none());
    assert!(
        fresh_binding
            .agent_catalog()
            .semantic_identity_v1()
            .is_some_and(|identity| !identity.is_empty())
    );

    let unchanged_binding = engine
        .bind_root_runtime(&workdir)
        .await
        .expect("bind unchanged catalog");
    assert_eq!(unchanged_binding.generation(), fresh_binding.generation());
}
