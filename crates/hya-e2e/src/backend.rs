//! Spawn and tear down a real `hya-backend serve` process.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::E2eError;

/// How long a backend gets to exit on its own after SIGTERM before the group is SIGKILLed.
///
/// Polled, not slept: a healthy backend is reaped in a few milliseconds, so the whole track
/// pays almost nothing. Only a backend that ignores SIGTERM reaches the full budget.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1_000);

/// Interval between `try_wait` polls while waiting out `SHUTDOWN_GRACE`.
const SHUTDOWN_POLL: Duration = Duration::from_millis(5);

/// Optional stdio MCP server registration for the temp config.
#[derive(Clone, Debug)]
pub struct McpFixture {
    /// Config key / MCP server name.
    pub name: String,
    /// Argv for the stdio MCP process (`command` + args).
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
    /// Additional model ids advertised by the fake provider.
    pub additional_models: Vec<String>,
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
    /// Spec with YOLO on, allow permissions, and FakeLlm defaults for model/provider.
    pub fn new(binary: impl Into<PathBuf>, fake_base_url: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            yolo: true,
            fake_base_url: fake_base_url.into(),
            model_id: "model".into(),
            additional_models: Vec::new(),
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
    /// Base URL of the listening server (no trailing path).
    pub url: String,
    /// Temp project workdir used as the session workdir.
    pub project: PathBuf,
    /// SQLite session db path for this process.
    pub db: PathBuf,
    /// Isolated `XDG_CONFIG_HOME` root.
    pub xdg_config_home: PathBuf,
    /// Same root used for XDG_DATA_HOME (bundle registry).
    pub xdg_data_home: PathBuf,
    /// Path to the `hya-backend` binary that was spawned.
    pub binary: PathBuf,
    child: Child,
    root: PathBuf,
    /// Model reference passed to the backend process on startup/reopen.
    model_ref: String,
    /// Whether the backend process receives `--yolo`.
    yolo: bool,
    /// Whether startup must eagerly connect configured sideplanes.
    has_mcp: bool,
    /// Set once the child has been signalled and reaped, so `Drop` does not signal a pid
    /// that the OS may already have handed to an unrelated process.
    stopped: bool,
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

        // Resolve relative MCP command paths against the project cwd so deferred
        // or absolute-spawned children still find fixture scripts.
        let mcp = resolve_mcp_commands(&spec.mcp, &project);
        let model_ref = format!("{}/{}", spec.provider_id, spec.model_id);
        let mut models = format!("    - id: {}\n", spec.model_id);
        for model in &spec.additional_models {
            models.push_str(&format!("    - id: {model}\n"));
        }
        let mcp_yaml = render_mcp(&mcp);
        let config = format!(
            r#"default_model: {model_ref}
providers:
  {provider}:
    kind: openai-compatible
    base_url: {base}
    api_key: e2e-test-key
    models:
{models}{mcp}
plugins: {{}}
permission:
  model: {perm}
  rules: []
"#,
            model_ref = model_ref,
            provider = spec.provider_id,
            base = spec.fake_base_url,
            models = models,
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
            // Lead its own process group so teardown can signal the whole tree (the backend
            // spawns MCP/plugin children). A group signal without this would hit the test
            // runner itself; a single-pid signal would orphan the grandchildren.
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // MCP connect is deferred by default; for process E2E we need tools in
        // the registry before the first FakeLlm tool call or they stay unknown.
        if !mcp.is_empty() {
            cmd.env("HYA_DEFER_SIDEPLANES", "0");
        }

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
            // Same graceful teardown as `Drop`: this child is a process-group leader and may
            // already have spawned MCP children, so a single-pid SIGKILL would orphan them.
            terminate_process_group(&mut child);
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
            model_ref,
            yolo: spec.yolo,
            has_mcp: !mcp.is_empty(),
            stopped: false,
        })
    }

    /// Stop and start the backend against the same project, config, and SQLite store.
    ///
    /// The FakeLlm remains owned by the surrounding [`E2eEnv`], so callers can
    /// compare replayed state without introducing another provider process.
    pub fn reopen(&mut self) -> Result<(), E2eError> {
        let _ = self.shutdown();

        let home = self.root.join("home");
        let state = self.root.join("state");
        let cache = self.root.join("cache");
        let mut cmd = Command::new(&self.binary);
        if self.yolo {
            cmd.arg("--yolo");
        }
        cmd.arg("--model")
            .arg(&self.model_ref)
            .arg("--db")
            .arg(&self.db)
            .arg("serve")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .current_dir(&self.project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CACHE_HOME", &cache)
            .env_remove("HYA_MODEL")
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.has_mcp {
            cmd.env("HYA_DEFER_SIDEPLANES", "0");
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| E2eError::Backend(format!("reopen hya-backend: {e}")))?;
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_process_group(&mut child);
                return Err(E2eError::Backend("reopen backend missing stdout".into()));
            }
        };
        let url = match wait_for_listen(stdout, Duration::from_secs(30)) {
            Ok(url) => url,
            Err(error) => {
                let stderr = child
                    .stderr
                    .as_mut()
                    .map(|stream| {
                        let mut buf = String::new();
                        let _ = std::io::Read::read_to_string(stream, &mut buf);
                        buf
                    })
                    .unwrap_or_default();
                terminate_process_group(&mut child);
                return Err(E2eError::Backend(format!(
                    "{error}; reopen stderr={stderr}"
                )));
            }
        };
        self.child = child;
        self.url = url;
        self.stopped = false;
        Ok(())
    }

    /// Stop the backend gracefully and return its exit status.
    ///
    /// `Drop` calls this too; it is public so a test can assert the status directly. A
    /// backend that returned from `main` reports `Some(0)`; one that died by signal reports
    /// `None` from `code()`, which is the discriminator for "atexit handlers did not run".
    pub fn shutdown(&mut self) -> Option<std::process::ExitStatus> {
        if self.stopped {
            return None;
        }
        self.stopped = true;
        terminate_process_group(&mut self.child)
    }

    /// Project workdir as a string for API `workdir` fields.
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
        self.shutdown();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Stop a backend: SIGTERM the group, poll for exit up to [`SHUTDOWN_GRACE`], then SIGKILL
/// the group unconditionally and reap. Returns the observed exit status.
///
/// The graceful first step exists so the child runs its atexit handlers — that is what
/// flushes LLVM `.profraw` data, and a SIGKILL'd backend contributes no coverage at all.
/// The SIGKILL escalation is deliberately **unconditional**: it is what preserves the
/// no-orphan guarantee for a backend that ignores SIGTERM, and it also reaps anything the
/// backend itself left behind in the group. This runs during panic unwinding too, so a
/// failing scenario still tears its backend down.
fn terminate_process_group(child: &mut Child) -> Option<std::process::ExitStatus> {
    // Signal the NEGATIVE pid to reach the whole group; the child was spawned with
    // `process_group(0)`, so this cannot reach the test runner.
    let pid = child.id() as libc::pid_t;
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    let mut status = None;
    loop {
        match child.try_wait() {
            // Reaped: return immediately rather than sleeping out the grace period.
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(SHUTDOWN_POLL),
            _ => break,
        }
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    status.or_else(|| child.wait().ok())
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

fn resolve_mcp_commands(mcp: &[McpFixture], project: &Path) -> Vec<McpFixture> {
    mcp.iter()
        .map(|server| {
            let command = server
                .command
                .iter()
                .map(|part| {
                    let candidate = project.join(part);
                    if candidate.is_file() {
                        candidate.display().to_string()
                    } else {
                        part.clone()
                    }
                })
                .collect();
            McpFixture {
                name: server.name.clone(),
                command,
            }
        })
        .collect()
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
            let url: String = rest.chars().take_while(|c| c.is_ascii_graphic()).collect();
            if url.starts_with("http://127.0.0.1:") {
                return Some(url);
            }
        }
    }
    None
}

/// Resolve the backend binary: `HYA_E2E_BACKEND_BIN` if set, else the workspace
/// `target/debug/hya-backend`.
///
/// The override exists so a coverage run can point the harness at an instrumented build
/// without overwriting the normal debug binary that concurrent work is using.
pub fn default_backend_bin() -> PathBuf {
    resolve_backend_bin(std::env::var_os("HYA_E2E_BACKEND_BIN"))
}

/// Pure resolution behind [`default_backend_bin`], so the override is testable without
/// mutating the process environment (which races other tests in the same binary).
fn resolve_backend_bin(override_bin: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(bin) = override_bin {
        let bin = PathBuf::from(bin);
        if !bin.as_os_str().is_empty() {
            return bin.canonicalize().unwrap_or(bin);
        }
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn backend_bin_override_wins_over_the_workspace_debug_path() {
        let resolved = resolve_backend_bin(Some("/nonexistent/instrumented/hya-backend".into()));

        assert_eq!(
            resolved,
            PathBuf::from("/nonexistent/instrumented/hya-backend"),
            "HYA_E2E_BACKEND_BIN must point the harness at an arbitrary build"
        );
    }

    #[test]
    fn empty_backend_bin_override_falls_back_to_the_workspace_debug_path() {
        let resolved = resolve_backend_bin(Some("".into()));

        assert_eq!(resolved, resolve_backend_bin(None));
    }

    /// The discriminator for the whole graceful-shutdown change: a backend that returned
    /// from `main` exits with status code 0, so its atexit handlers ran (which is what
    /// flushes LLVM `.profraw`). A backend killed by a signal has `code() == None`.
    ///
    /// This also covers the readiness race: SIGTERM is sent immediately after the listen
    /// line, so the signal handler must already be installed when that line is printed.
    #[test]
    fn shutdown_stops_the_backend_cleanly_with_exit_status_zero() {
        let binary = default_backend_bin();
        assert!(
            binary.is_file(),
            "build the backend first: cargo build -p hya-backend --bin hya-backend (looked at {})",
            binary.display()
        );
        // The fake provider is never contacted: this scenario only boots and stops.
        let spec = BackendSpec::new(binary, "http://127.0.0.1:1/v1");
        let mut backend = BackendProcess::start(&spec).expect("backend starts");

        let status = backend.shutdown().expect("backend reports an exit status");

        assert_eq!(
            status.code(),
            Some(0),
            "backend must return from main on SIGTERM, not die by signal; \
             a signal death means atexit handlers (and the coverage flush) were skipped"
        );
    }
}
