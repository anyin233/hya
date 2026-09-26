//! Remote model cache (`$XDG_CACHE_HOME/hya/model_cache.db`) and its merge
//! with config `models:` entries at startup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, MutexGuard};

/// Serialize env-sensitive async tests; an async-aware lock so the guard may
/// legitimately be held across `config::load().await`.
async fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: integration tests run serially for this crate's env-sensitive cases.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn unique_root(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let path = std::env::temp_dir().join(format!("hya-app-{label}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_hya_config(root: &Path, config_yaml: &str) -> PathBuf {
    let config_home = root.join("config");
    let hya = config_home.join("hya");
    std::fs::create_dir_all(&hya).unwrap();
    std::fs::write(hya.join("config.yaml"), config_yaml).unwrap();
    config_home
}

/// Isolated XDG config + cache + HOME for one test.
struct Isolated {
    root: PathBuf,
    config_home: PathBuf,
    cache_home: PathBuf,
    _guards: Vec<EnvGuard>,
}

fn isolated(label: &str, config_yaml: &str) -> Isolated {
    let root = unique_root(label);
    let config_home = write_hya_config(&root, config_yaml);
    let cache_home = root.join("cache");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let guards = vec![
        EnvGuard::set("XDG_CONFIG_HOME", config_home.to_str().unwrap()),
        EnvGuard::set("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
        EnvGuard::set("HOME", home.to_str().unwrap()),
    ];
    Isolated {
        root,
        config_home,
        cache_home,
        _guards: guards,
    }
}

async fn seed(provider: &str, rows: Vec<hya_app::model_cache::CachedModel>) {
    let cache = hya_app::model_cache::ModelCache::open_default()
        .await
        .expect("open cache");
    cache.replace_provider(provider, &rows).await.expect("seed");
    cache.close().await;
}

fn row(id: &str) -> hya_app::model_cache::CachedModel {
    hya_app::model_cache::CachedModel {
        id: id.to_string(),
        tools: true,
        fetched_at_ms: 1,
        ..hya_app::model_cache::CachedModel::default()
    }
}

/// Warm cache restores effort variants, context window, max output, and the
/// display name — not just bare model ids — without waiting on discovery.
#[tokio::test]
async fn load_restores_cached_metadata_without_waiting_on_unreachable_discovery() {
    let _env = env_lock().await;
    let env = isolated(
        "cache-rich",
        "default_model: gateway/rich-model\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
    );
    seed(
        "gateway",
        vec![
            hya_app::model_cache::CachedModel {
                display_name: Some("Rich Model".into()),
                context_limit: 128_000,
                output_limit: 16_384,
                reasoning_default: Some("medium".into()),
                reasoning_variants: vec![
                    "none".into(),
                    "low".into(),
                    "medium".into(),
                    "high".into(),
                ],
                ..row("rich-model")
            },
            row("other-model"),
        ],
    )
    .await;

    let started = Instant::now();
    let loaded = hya_app::config::load()
        .await
        .expect("load succeeds")
        .expect("config present");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cache hit must not wait on discovery; elapsed={:?}",
        started.elapsed()
    );
    let model = loaded
        .catalog
        .models()
        .iter()
        .find(|model| model.model_id == "rich-model")
        .expect("rich-model row");
    assert_eq!(model.capabilities.max_context, 128_000);
    assert_eq!(model.capabilities.max_output, 16_384);
    assert_eq!(model.display_name.as_deref(), Some("Rich Model"));
    assert_eq!(model.source, hya_provider::ModelCatalogSource::Discovered);
    assert_eq!(
        model.reasoning_variants,
        vec!["none", "low", "medium", "high"]
    );
    assert_eq!(
        model.reasoning_default,
        Some(hya_provider::ReasoningEffort::Medium)
    );
    assert!(
        loaded
            .catalog
            .models()
            .iter()
            .any(|model| model.model_id == "other-model")
    );
    // A discovery-only provider still refreshes in the background.
    assert_eq!(loaded.pending_discovery.len(), 1);
    assert!(env.cache_home.join("hya/model_cache.db").is_file());
    let _ = std::fs::remove_dir_all(&env.root);
}

/// A non-empty `models:` no longer disables the remote list: cached remote
/// rows and config entries merge per id, config fields winning.
#[tokio::test]
async fn pinned_provider_merges_cached_remote_rows_with_config_entries() {
    let _env = env_lock().await;
    let env = isolated(
        "cache-merge",
        "default_model: gateway/shared\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models:\n      - id: shared\n        name: Shared (config)\n        limit:\n          output: 2048\n      - pinned\n",
    );
    seed(
        "gateway",
        vec![
            hya_app::model_cache::CachedModel {
                display_name: Some("Shared (remote)".into()),
                context_limit: 64_000,
                output_limit: 8_192,
                ..row("shared")
            },
            row("remote"),
        ],
    )
    .await;
    let loaded = hya_app::config::load().await.unwrap().unwrap();
    let find = |id: &str| {
        loaded
            .catalog
            .models()
            .iter()
            .find(|model| model.model_id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
            .clone()
    };
    use hya_provider::ModelCatalogSource as S;
    let shared = find("shared");
    assert_eq!(shared.source, S::Overridden);
    assert_eq!(shared.display_name.as_deref(), Some("Shared (config)"));
    assert_eq!(shared.capabilities.max_context, 64_000);
    assert_eq!(shared.capabilities.max_output, 2_048);
    assert_eq!(find("remote").source, S::Discovered);
    assert_eq!(find("pinned").source, S::Configured);
    assert!(
        loaded.pending_discovery.is_empty(),
        "a pinned provider with cached rows does not refresh at startup"
    );
    let _ = std::fs::remove_dir_all(&env.root);
}

/// A pinned provider with no cached rows starts from config at once and is
/// queued for background discovery.
#[tokio::test]
async fn pinned_provider_without_cache_rows_is_queued_for_discovery() {
    let _env = env_lock().await;
    let env = isolated(
        "cache-pinned-miss",
        "default_model: gateway/pinned\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: [pinned]\n",
    );
    let started = Instant::now();
    let loaded = hya_app::config::load().await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(loaded.pending_discovery.len(), 1);
    assert_eq!(loaded.catalog.default_model().as_str(), "gateway/pinned");
    let _ = std::fs::remove_dir_all(&env.root);
}

/// The legacy `models.yml.cache` is imported once into the database and is
/// never rewritten.
#[tokio::test]
async fn legacy_models_yml_cache_is_imported_once_and_left_untouched() {
    let _env = env_lock().await;
    let env = isolated(
        "cache-legacy",
        "default_model: gateway/legacy-model\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
    );
    let legacy = env.config_home.join("hya/models.yml.cache");
    let yaml = "version: 1\nproviders:\n  gateway:\n    - id: legacy-model\n      limit:\n        context: 64000\n        output: 4096\n";
    std::fs::write(&legacy, yaml).unwrap();
    let config_bytes = std::fs::read(env.config_home.join("hya/config.yaml")).unwrap();

    let loaded = hya_app::config::load().await.unwrap().unwrap();
    let model = loaded
        .catalog
        .models()
        .iter()
        .find(|model| model.model_id == "legacy-model")
        .expect("imported row");
    assert_eq!(model.capabilities.max_context, 64_000);
    assert_eq!(model.capabilities.max_output, 4_096);
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), yaml);
    assert_eq!(
        std::fs::read(env.config_home.join("hya/config.yaml")).unwrap(),
        config_bytes
    );

    let cache = hya_app::model_cache::ModelCache::open_default()
        .await
        .unwrap();
    assert_eq!(
        cache.provider_models("gateway").await.unwrap()[0].id,
        "legacy-model"
    );
    cache.close().await;
    let _ = std::fs::remove_dir_all(&env.root);
}
