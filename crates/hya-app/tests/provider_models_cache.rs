//! Provider model catalog cache beside `config.yaml`.

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

/// Warm cache restores effort variants, context window, and max output — not
/// just bare model ids.
#[tokio::test]
async fn load_restores_effort_context_and_max_output_from_models_yml_cache() {
    let _env = env_lock().await;
    let root = unique_root("provider-cache-rich");
    let config_home = write_hya_config(
        &root,
        "default_model: gateway/rich-model\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
    );
    let _xdg = EnvGuard::set("XDG_CONFIG_HOME", config_home.to_str().unwrap());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", home.to_str().unwrap());

    let mut file = hya_app::config::ModelsCacheFile::default();
    file.providers.insert(
        "gateway".into(),
        vec![hya_app::config::CachedModelEntry {
            id: "rich-model".into(),
            limit: hya_app::config::CachedModelLimit {
                context: 128_000,
                output: 16_384,
            },
            reasoning_default: Some("medium".into()),
            reasoning_variants: vec!["none".into(), "low".into(), "medium".into(), "high".into()],
            tools: true,
        }],
    );
    hya_app::config::write_models_cache_file(&file).expect("seed rich cache");

    let loaded = hya_app::config::load()
        .await
        .expect("load succeeds")
        .expect("config present");
    let model = loaded
        .catalog
        .models()
        .iter()
        .find(|model| model.model_id == "rich-model")
        .expect("rich-model row");
    assert_eq!(model.capabilities.max_context, 128_000);
    assert_eq!(model.capabilities.max_output, 16_384);
    assert_eq!(
        model.reasoning_variants,
        vec!["none", "low", "medium", "high"]
    );
    assert_eq!(
        model.reasoning_default,
        Some(hya_provider::ReasoningEffort::Medium)
    );
    let written = std::fs::read_to_string(config_home.join("hya/models.yml.cache")).unwrap();
    assert!(
        written.contains("context"),
        "cache must persist limit metadata, got:\n{written}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Warm `models.yml.cache` must populate the startup catalog without waiting on
/// unreachable discovery endpoints.
#[tokio::test]
async fn load_uses_models_yml_cache_without_waiting_on_unreachable_discovery() {
    let _env = env_lock().await;
    let root = unique_root("provider-cache-warm");
    let config_home = write_hya_config(
        &root,
        "default_model: gateway/cached-model\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
    );
    let _xdg = EnvGuard::set("XDG_CONFIG_HOME", config_home.to_str().unwrap());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", home.to_str().unwrap());
    let mut file = hya_app::config::ModelsCacheFile::default();
    file.providers.insert(
        "gateway".into(),
        vec![
            hya_app::config::CachedModelEntry {
                id: "cached-model".into(),
                ..hya_app::config::CachedModelEntry::default()
            },
            hya_app::config::CachedModelEntry {
                id: "other-model".into(),
                ..hya_app::config::CachedModelEntry::default()
            },
        ],
    );
    hya_app::config::write_models_cache_file(&file).expect("seed cache");

    let started = Instant::now();
    let loaded = hya_app::config::load()
        .await
        .expect("load succeeds")
        .expect("config present");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "cache hit must not wait on discovery timeout; elapsed={:?}",
        started.elapsed()
    );
    let ids: Vec<String> = loaded
        .catalog
        .models()
        .iter()
        .map(|model| model.model_ref().to_string())
        .collect();
    assert!(
        ids.iter().any(|id| id == "gateway/cached-model"),
        "missing cached-model in {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id == "gateway/other-model"),
        "missing other-model in {ids:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Successful discovery must rewrite `models.yml.cache` and leave `config.yaml` untouched.
#[tokio::test]
async fn discovery_refresh_writes_models_yml_cache_without_mutating_config() {
    let _env = env_lock().await;
    let root = unique_root("provider-cache-write");
    let config_home = write_hya_config(
        &root,
        "default_model: gateway/live\nproviders:\n  gateway:\n    kind: openai\n    base_url: http://127.0.0.1:9/v1\n    models: []\n",
    );
    let cache_path = config_home.join("hya/models.yml.cache");
    let config_path = config_home.join("hya/config.yaml");
    let config_bytes = std::fs::read(&config_path).unwrap();
    let _xdg = EnvGuard::set("XDG_CONFIG_HOME", config_home.to_str().unwrap());
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvGuard::set("HOME", home.to_str().unwrap());

    let mut file = hya_app::config::ModelsCacheFile::default();
    file.providers.insert(
        "gateway".into(),
        vec![
            hya_app::config::CachedModelEntry {
                id: "live-a".into(),
                limit: hya_app::config::CachedModelLimit {
                    context: 64_000,
                    output: 4_096,
                },
                ..hya_app::config::CachedModelEntry::default()
            },
            hya_app::config::CachedModelEntry {
                id: "live-b".into(),
                ..hya_app::config::CachedModelEntry::default()
            },
        ],
    );
    hya_app::config::write_models_cache_file(&file).expect("write cache");
    assert!(
        cache_path.is_file(),
        "cache file missing at {}",
        cache_path.display()
    );
    let cache = std::fs::read_to_string(&cache_path).unwrap();
    assert!(cache.contains("live-a"), "{cache}");
    assert!(cache.contains("live-b"), "{cache}");
    assert!(cache.contains("context"), "{cache}");
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);
    let _ = std::fs::remove_dir_all(root);
}
