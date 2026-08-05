//! Spawn and tear down a real `hya-backend serve` process.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::E2eError;

/// Optional stdio MCP server registration for the temp config.
#[derive(Clone, Debug)]
pub struct McpFixture {
    pub name: String,
    pub command: Vec<String>,
}

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
    /// `permission.model` in config.yaml (`allow` | `default` | `strict`).
    pub permission_model: String,
    /// Optional MCP servers written into config.
    pub mcp: Vec<McpFixture>,
    /// Optional project-relative skill files written before boot (`path` under project, body).
    pub skill_files: Vec<(String, String)>,
    /// Optional files written into the project before boot.
    pub project_files: Vec<(String, Vec<u8>)>,
    /// Hyabundle package paths to install into XDG_DATA_HOME before serve.
    pub preinstall_bundles: Vec<PathBuf>,
}

impl BackendSpec {
    pub fn new(binary: impl Into<PathBuf>, fake_base_url: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            yolo: true,
            fake_base_url: fake_base_url.into(),
            model_id: "model".into(),
            provider_id: "fake".into(),
            permission_model: "allow".into(),
            mcp: Vec::new(),
            skill_files: Vec::new(),
            project_files: Vec::new(),
            preinstall_bundles: Vec::new(),
        }
    }
}

/// Running backend + isolation roots.
pub struct BackendProcess {
    pub url: String,
    pub project: PathBuf,
    pub db: PathBuf,
    pub xdg_config_home: PathBuf,
    /// Same root used for XDG_DATA_HOME (bundle registry).
    pub xdg_data_home: PathBuf,
    pub binary: PathBuf,
    child: Child,
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
        let xdg_data_home = root.join("data");
        let xdg_state = root.join("state");
        let xdg_cache = root.join("cache");
        let hya_cfg = xdg_config_home.join("hya");
        let auth_dir = hya_cfg.join("auth");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&auth_dir)?;
        std::fs::create_dir_all(&xdg_data_home)?;
        std::fs::create_dir_all(&xdg_state)?;
        std::fs::create_dir_all(&xdg_cache)?;
        std::fs::write(project.join("README.md"), "hya-e2e project\n")?;

        for (rel, body) in &spec.project_files {
            let path = project.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, body)?;
        }
        for (rel, body) in &spec.skill_files {
            let path = project.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, body)?;
        }

        let model_ref = format!("{}/{}", spec.provider_id, spec.model_id);
        let mcp_yaml = render_mcp(&spec.mcp);
        let config = format!(
            r#"default_model: {model_ref}
providers:
  {provider}:
    kind: openai-compatible
    base_url: {base}
    api_key: e2e-test-key
    models:
    - id: {model}
{mcp}
plugins: {{}}
permission:
  model: {perm}
  rules: []
"#,
            model_ref = model_ref,
            provider = spec.provider_id,
            base = spec.fake_base_url,
            model = spec.model_id,
            mcp = mcp_yaml,
            perm = spec.permission_model,
        );
        std::fs::write(hya_cfg.join("config.yaml"), config)?;
        std::fs::write(
            auth_dir.join(format!("{}.yaml", spec.provider_id)),
            "token: e2e-test-key\n",
        )?;

        for package in &spec.preinstall_bundles {
            let output = Command::new(&spec.binary)
                .args(["bundle", "install"])
                .arg(package)
                .env("XDG_DATA_HOME", &xdg_data_home)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", &xdg_config_home)
                .output()
                .map_err(|e| E2eError::Backend(format!("preinstall bundle spawn: {e}")))?;
            if !output.status.success() {
                return Err(E2eError::Backend(format!(
                    "preinstall bundle {} failed: status={} stdout={} stderr={}",
                    package.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                )));
            }
        }

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
            .env("XDG_DATA_HOME", &xdg_data_home)
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
        let url = wait_for_listen(stdout, Duration::from_secs(30)).map_err(|e| {
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
            xdg_data_home,
            binary: spec.binary.clone(),
            child,
            root,
        })
    }

    #[must_use]
    pub fn workdir_str(&self) -> String {
        self.project.display().to_string()
    }

    /// Run `hya-backend bundle …` with this process's data home.
    pub fn bundle_cli(&self, args: &[&str]) -> Result<std::process::Output, E2eError> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(args)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .current_dir(&self.project);
        cmd.output()
            .map_err(|e| E2eError::Backend(format!("bundle cli: {e}")))
    }
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn render_mcp(mcp: &[McpFixture]) -> String {
    if mcp.is_empty() {
        return "mcp: {}".to_string();
    }
    let mut out = String::from("mcp:\n");
    for server in mcp {
        out.push_str(&format!("  {}:\n", server.name));
        let quoted = server
            .command
            .iter()
            .map(|part| format!("\"{}\"", part.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("    command: [{quoted}]\n"));
        out.push_str("    enabled: true\n");
    }
    out
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
    for line in text.lines() {
        if let Some(idx) = line.find("http://127.0.0.1:") {
            let rest = &line[idx..];
            let url: String = rest
                .chars()
                .take_while(|c| c.is_ascii_graphic())
                .collect();
            if url.starts_with("http://127.0.0.1:") {
                return Some(url);
            }
        }
    }
    None
}

/// Resolve default debug binary relative to workspace (from this crate).
pub fn default_backend_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/hya-backend")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/hya-backend")
        })
}

/// Relative project path for the stdio MCP echo fixture written by the harness.
pub const MCP_ECHO_SCRIPT_REL: &str = "fixtures/mcp_echo.py";

/// Python source for a minimal JSON-RPC stdio MCP echo server (`ping` tool).
pub fn mcp_echo_script() -> &'static str {
    r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "echo", "version": "0.0.1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": "ping",
                    "description": "Ping echo",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"msg": {"type": "string"}},
                    },
                }
            ]
        }
    elif method == "tools/call":
        args = (req.get("params") or {}).get("arguments") or {}
        msg = args.get("msg", "pong")
        result = {
            "content": [{"type": "text", "text": f"echo:{msg}"}],
            "isError": False,
        }
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#
}

/// Command argv for the project-local MCP echo fixture (`cwd` = project root).
pub fn mcp_echo_command() -> Vec<String> {
    vec!["python3".into(), MCP_ECHO_SCRIPT_REL.into()]
}

/// Absolute path to the public package bytes used by Track P bundle scenarios.
pub fn public_bundle_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z")
        })
}

/// Copy the public fixture into `dest_dir` with the required `.hyabundle` suffix.
pub fn materialize_public_bundle(dest_dir: &Path) -> Result<PathBuf, E2eError> {
    std::fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join("demo.hyabundle");
    std::fs::copy(public_bundle_source(), &dest)?;
    Ok(dest)
}

/// Absolute path helper kept for callers that only need the source archive.
pub fn public_bundle_fixture() -> PathBuf {
    public_bundle_source()
}
