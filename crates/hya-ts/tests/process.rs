#![allow(clippy::unwrap_used)]

use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn direct_help_uses_hya_ts_branding() {
    let output = Command::new(env!("CARGO_BIN_EXE_hya-ts"))
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: hya-ts "), "{stdout}");
}

#[test]
fn direct_version_uses_hya_ts_branding() {
    let output = Command::new(env!("CARGO_BIN_EXE_hya-ts"))
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("hya-ts {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn import_compat_exits_before_tui_processes_start() {
    let fixture = Fixture::new("import");
    let compat_config = fixture.root.join("opencode.json");
    let xdg_config = fixture.root.join("xdg-config");
    std::fs::create_dir(&xdg_config).unwrap();
    std::fs::write(
        &compat_config,
        r#"{
  "model": "gateway/model",
  "provider": {
    "gateway": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "https://gateway.example/v1", "apiKey": "test" },
      "models": { "model": {} }
    }
  }
}"#,
    )
    .unwrap();

    let output = fixture
        .command()
        .args([
            fixture.root.join("missing-project").as_os_str(),
            "--backend-bin".as_ref(),
            fixture.root.join("missing-backend").as_os_str(),
            "--bun".as_ref(),
            fixture.root.join("missing-bun").as_os_str(),
            "--import".as_ref(),
            "compat".as_ref(),
        ])
        .env("COMPAT_CONFIG", compat_config)
        .env("XDG_CONFIG_HOME", &xdg_config)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("imported 1 providers and 1 models"),
        "{output:?}"
    );
    assert!(xdg_config.join("hya/config.yaml").is_file());
    assert!(!fixture.args.exists(), "Bun must not start during import");
    assert!(
        !fixture.backend_seen.exists(),
        "the backend must not start during import"
    );
}

#[test]
fn missing_bun_error_names_attempted_executable() {
    let fixture = Fixture::new("missing-bun");
    let missing_bun = fixture.root.join("missing-bun");
    let output = fixture
        .command()
        .args([
            fixture.project.as_os_str(),
            "--server".as_ref(),
            "http://127.0.0.1:54321".as_ref(),
            "--bun".as_ref(),
            missing_bun.as_os_str(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("hya-ts: failed to launch Bun"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&missing_bun.display().to_string()),
        "{output:?}"
    );
    assert!(!fixture.args.exists(), "missing Bun must not fall back");
}

#[test]
fn attached_mode_forwards_arguments_propagates_status_and_leaves_server_alive() {
    let fixture = Fixture::new("attached");
    let mut server = Command::new("sleep")
        .arg("300")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server_pid = server.id();

    let output = fixture
        .command()
        .args([
            fixture.project.as_os_str(),
            "--server".as_ref(),
            "http://127.0.0.1:54321".as_ref(),
            "--bun".as_ref(),
            fixture.bun.as_os_str(),
            "--continue".as_ref(),
            "--session".as_ref(),
            "ses_attached".as_ref(),
        ])
        .env("BUN_EXIT_CODE", "23")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(23), "{output:?}");
    assert!(process_alive(server_pid));
    server.kill().unwrap();
    server.wait().unwrap();
    assert_eq!(
        std::fs::read_to_string(&fixture.cwd).unwrap().trim(),
        fixture.runtime.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        lines(&fixture.args),
        vec![
            "src/main.tsx",
            "--url",
            "http://127.0.0.1:54321",
            "--project",
            fixture.project.canonicalize().unwrap().to_str().unwrap(),
            "--continue",
            "--session",
            "ses_attached",
        ]
    );
}

#[test]
fn owned_mode_drops_server_handle_and_announced_port_after_bun_exit() {
    let fixture = Fixture::new("owned");
    let backend = fixture.fake_backend();
    let output = fixture
        .command()
        .args([
            fixture.project.as_os_str(),
            "--backend-bin".as_ref(),
            backend.as_os_str(),
            "--bun".as_ref(),
            fixture.bun.as_os_str(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&fixture.backend_seen).unwrap(),
        "alive\n"
    );
    let pid = read_u32(&fixture.backend_pid);
    let child_pid = read_u32(&fixture.backend_child_pid);
    let port = read_u16(&fixture.backend_port);
    wait_until(|| !process_alive(pid) && !process_alive(child_pid));
    assert!(
        !process_alive(pid),
        "owned backend {pid} survived launcher exit"
    );
    assert!(
        !process_alive(child_pid),
        "backend child {child_pid} survived launcher exit"
    );
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[test]
fn handled_termination_stops_bun_and_owned_backend() {
    let fixture = Fixture::new("signal");
    let backend = fixture.fake_backend();
    let mut launcher = fixture
        .command()
        .args([
            fixture.project.as_os_str(),
            "--backend-bin".as_ref(),
            backend.as_os_str(),
            "--bun".as_ref(),
            fixture.bun.as_os_str(),
        ])
        .env("BUN_WAIT", "300")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    wait_until(|| fixture.backend_seen.exists());
    assert!(
        fixture.backend_seen.exists(),
        "Bun never observed the backend"
    );
    unsafe { libc::kill(launcher.id() as libc::pid_t, libc::SIGTERM) };
    let status = launcher.wait().unwrap();

    assert_eq!(status.code(), Some(1));
    let pid = read_u32(&fixture.backend_pid);
    let child_pid = read_u32(&fixture.backend_child_pid);
    let port = read_u16(&fixture.backend_port);
    wait_until(|| !process_alive(pid) && !process_alive(child_pid));
    assert!(!process_alive(pid));
    assert!(!process_alive(child_pid));
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn attached_mode_gives_bun_the_terminal_and_restores_it() {
    let fixture = Fixture::new("pty");
    executable(
        &fixture.bun,
        "#!/bin/sh\npgid=$(ps -o pgid= -p $$ | tr -d ' ')\nforeground=$(ps -o tpgid= -p $$ | tr -d ' ')\nprintf 'bun pgid=%s foreground=%s\\n' \"$pgid\" \"$foreground\"\n[ \"$pgid\" = \"$foreground\" ] || exit 91\nstty -echo\nprintf 'bun changed terminal\\n'\n",
    );
    let transcript = fixture.root.join("pty-transcript");
    let command = format!(
        r#"stty rows 30 cols 100; before=$(stty -g); before_fg=$(ps -o tpgid= -p $$ | tr -d ' '); "{}" "{}" --server http://127.0.0.1:54321 --bun "{}"; code=$?; after=$(stty -g); after_fg=$(ps -o tpgid= -p $$ | tr -d ' '); [ "$code" -eq 0 ] || exit "$code"; [ "$before" = "$after" ] || exit 97; [ "$before_fg" = "$after_fg" ] || exit 98"#,
        env!("CARGO_BIN_EXE_hya-ts"),
        fixture.project.display(),
        fixture.bun.display(),
    );
    let output = Command::new("/usr/bin/script")
        .args(["-q", "-e", "-f", "-c", &command])
        .arg(&transcript)
        .env("HYA_TUI_TS_DIR", &fixture.runtime)
        .output()
        .unwrap();

    let transcript = std::fs::read_to_string(transcript).unwrap();
    assert!(output.status.success(), "{output:?}\n{transcript}");
    assert!(transcript.contains("bun changed terminal"), "{transcript}");
}

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    runtime: PathBuf,
    bun: PathBuf,
    args: PathBuf,
    cwd: PathBuf,
    backend_pid: PathBuf,
    backend_child_pid: PathBuf,
    backend_port: PathBuf,
    backend_seen: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = temp_dir(&format!("hya-ts-{label}"));
        let project = root.join("project");
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(runtime.join("src")).unwrap();
        std::fs::write(runtime.join("src/main.tsx"), "").unwrap();
        let args = root.join("bun-args");
        let cwd = root.join("bun-cwd");
        let backend_pid = root.join("backend-pid");
        let backend_child_pid = root.join("backend-child-pid");
        let backend_port = root.join("backend-port");
        let backend_seen = root.join("backend-seen");
        let bun = root.join("bun");
        executable(
            &bun,
            &format!(
                "#!/bin/sh\npwd > '{}'\nprintf '%s\\n' \"$@\" > '{}'\nif [ -n \"$BACKEND_PID_FILE\" ] && kill -0 \"$(cat \"$BACKEND_PID_FILE\")\" 2>/dev/null; then printf 'alive\\n' > '{}'; fi\nif [ -n \"$BUN_WAIT\" ]; then trap 'exit 0' TERM INT; sleep \"$BUN_WAIT\" & wait $!; fi\nexit \"${{BUN_EXIT_CODE:-0}}\"\n",
                cwd.display(),
                args.display(),
                backend_seen.display()
            ),
        );
        Self {
            root,
            project,
            runtime,
            bun,
            args,
            cwd,
            backend_pid,
            backend_child_pid,
            backend_port,
            backend_seen,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hya-ts"));
        command
            .env("HYA_TUI_TS_DIR", &self.runtime)
            .env("BACKEND_PID_FILE", &self.backend_pid)
            .env("BACKEND_CHILD_PID_FILE", &self.backend_child_pid)
            .env("BACKEND_PORT_FILE", &self.backend_port);
        command
    }

    fn fake_backend(&self) -> PathBuf {
        let path = self.root.join("hya-backend");
        executable(
            &path,
            r#"#!/usr/bin/env python3
import os, signal, socket, subprocess, sys, time
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", 0))
s.listen()
port = s.getsockname()[1]
open(os.environ["BACKEND_PID_FILE"], "w").write(str(os.getpid()))
child = subprocess.Popen(["sleep", "300"])
open(os.environ["BACKEND_CHILD_PID_FILE"], "w").write(str(child.pid))
open(os.environ["BACKEND_PORT_FILE"], "w").write(str(port))
print(f"listening on http://127.0.0.1:{port}", flush=True)
def stop(signum, frame):
    s.close()
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
while True:
    time.sleep(1)
"#,
        );
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_u32(path: &Path) -> u32 {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

fn read_u16(path: &Path) -> u16 {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}
