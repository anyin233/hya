//! Spawn and tear down a real `hya-backend serve` process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::E2eError;

/// How to configure the temp backend process.
#[derive(Clone, Debug)]
pub struct BackendSpec {
    /// Absolute path to `hya-backend` binary.
    pub binary: PathBuf,
    /// When true, pass `--yolo` (auto-approve tools).
    pub yolo: bool,
    /// OpenAI-compatible base URL including `/v1` suffix (FakeLlm base_url).
    pub fake_base_url: String,
    /// Model id served by the fake provider (default `model`).
    pub model_id: String,
    /// Provider id in config (default `fake`).
    pub provider_id: String,
}

impl BackendSpec {
    pub fn new(binary: impl Into<PathBuf>, fake_base_url: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            yolo: true,
            fake_base_url: fake_base_url.into(),
            model_id: "model".into(),
            provider_id: "fake".into(),
        }
    }
}

/// Running backend + isolation roots.
pub struct BackendProcess {
    pub url: String,
    pub project: PathBuf,
    pub db: PathBuf,
    pub xdg_config_home: PathBuf,
    child: Child,
    home: PathBuf,
    root: PathBuf,
}

impl BackendProcess {
    /// Create temp dirs, write config/auth, spawn serve on ephemeral port.
    pub fn start(spec: &BackendSpec) -> Result<Self, E2eError> {
        if !spec.binary.is_file() {
            return Err(E2eError::Backend(format!(
                "hya-backend binary missing at {} (run: cargo build -p hya-backend --bin hya-backend)",
                spec.binary.display()
            )));
        }
        let root = temp_root("hya-e2e")?;
        let project = root.join("project");
        let home = root.join("home");
        let xdg_config_home = root.join("config");
        let xdg_state = root.join("state");
        let xdg_cache = root.join("cache");
        let hya_cfg = xdg_config_home.join("hya");
        let auth_dir = hya_cfg.join("auth");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&auth_dir)?;
        std::fs::create_dir_all(&xdg_state)?;
        std::fs::create_dir_all(&xdg_cache)?;
        std::fs::write(project.join("README.md"), "hya-e2e project\n")?;

        let model_ref = format!("{}/{}", spec.provider_id, spec.model_id);
        let config = format!(
            r#"default_model: {model_ref}
providers:
  {provider}:
    kind: openai-compatible
    base_url: {base}
    api_key: e2e-test-key
    models:
    - id: {model}
mcp: {{}}
plugins: {{}}
permission:
  model: allow
  rules: []
"#,
            model_ref = model_ref,
            provider = spec.provider_id,
            base = spec.fake_base_url,
            model = spec.model_id,
        );
        std::fs::write(hya_cfg.join("config.yaml"), config)?;
        std::fs::write(
            auth_dir.join(format!("{}.yaml", spec.provider_id)),
            "token: e2e-test-key\n",
        )?;

        let db = root.join("sessions.db");
        let mut cmd = Command::new(&spec.binary);
        if spec.yolo {
            cmd.arg("--yolo");
        }
        cmd.arg("--model")
            .arg(&model_ref)
            .arg("--db")
            .arg(&db)
            .arg("serve")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .current_dir(&project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &xdg_config_home)
            .env("XDG_STATE_HOME", &xdg_state)
            .env("XDG_CACHE_HOME", &xdg_cache)
            .env_remove("HYA_MODEL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| E2eError::Backend(format!("spawn hya-backend: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| E2eError::Backend("missing stdout".into()))?;
        let url = wait_for_listen(stdout, Duration::from_secs(20)).map_err(|e| {
            let stderr = child
                .stderr
                .as_mut()
                .map(|s| {
                    let mut buf = String::new();
                    let _ = std::io::Read::read_to_string(s, &mut buf);
                    buf
                })
                .unwrap_or_default();
            let _ = child.kill();
            E2eError::Backend(format!("{e}; stderr={stderr}"))
        })?;

        Ok(Self {
            url,
            project,
            db,
            xdg_config_home,
            child,
            home,
            root,
        })
    }

    #[must_use]
    pub fn workdir_str(&self) -> String {
        self.project.display().to_string()
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = self.home;
        let _ = self.xdg_config_home;
    }
}

fn temp_root(label: &str) -> Result<PathBuf, E2eError> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn wait_for_listen(mut stdout: impl std::io::Read, timeout: Duration) -> Result<String, E2eError> {
    let start = Instant::now();
    let mut buf = [0u8; 1024];
    let mut acc = String::new();
    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::Backend(format!(
                "timeout waiting for listen line; got: {acc}"
            )));
        }
        // Blocking read; spawn is piped and readiness is immediate after bind.
        match std::io::Read::read(&mut stdout, &mut buf) {
            Ok(0) => {
                return Err(E2eError::Backend(format!(
                    "backend exited before readiness; output={acc}"
                )));
            }
            Ok(n) => {
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if let Some(url) = parse_listen_url(&acc) {
                    return Ok(url);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(E2eError::Backend(format!("read stdout: {e}"))),
        }
    }
}

fn parse_listen_url(text: &str) -> Option<String> {
    // hya server listening on http://127.0.0.1:PORT
    for line in text.lines() {
        if let Some(idx) = line.find("http://127.0.0.1:") {
            let rest = &line[idx..];
            let url: String = rest.chars().take_while(|c| c.is_ascii_graphic()).collect();
            if url.starts_with("http://127.0.0.1:") {
                return Some(url);
            }
        }
    }
    None
}

/// Resolve default debug binary relative to workspace (from this crate).
pub fn default_backend_bin() -> PathBuf {
    // crates/hya-e2e -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/hya-backend")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/hya-backend")
        })
}
