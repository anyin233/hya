//! Shared scratch-dir, subprocess, readiness, and daemon helpers for `tests/it/`.
#![allow(dead_code)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Resolve the `hya` binary path, preferring nextest's runtime-remapped
/// `NEXTEST_BIN_EXE_hya` so archived test runs work across directories.
pub fn hya_bin() -> String {
    std::env::var("NEXTEST_BIN_EXE_hya")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_hya"))
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_hya").to_owned())
}

pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{serial}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir.canonicalize()?))
    }

    pub fn db(&self) -> PathBuf {
        self.0.join("state/sessions.db")
    }

    pub fn hya(&self) -> Command {
        let mut command = Command::new(hya_bin());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("NO_COLOR", "1")
            .current_dir(&self.0)
            .stdin(Stdio::null());
        command
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn combined_text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed: {}",
        combined_text(output)
    );
}

pub fn wait_until(what: &str, limit: Duration, mut check: impl FnMut() -> bool) -> TestResult {
    let deadline = Instant::now() + limit;
    while !check() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting until {what}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

pub fn run_bounded(
    cmd: &mut Command,
    limit: Duration,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = BufReader::new(stdout).read_to_end(&mut buf);
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let out_s = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
            let err_s = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
            return Err(format!(
                "command did not exit within {limit:?}; stdout={out_s:?} stderr={err_s:?}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        status,
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}

pub struct ForegroundServer {
    pub child: Child,
    pub url: String,
}

impl ForegroundServer {
    pub fn start(
        mut command: Command,
        ready_prefix: &'static str,
        discovery: Option<&Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix(ready_prefix) {
                    let _ = tx.send(url.trim().to_string());
                }
            }
        });
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
        }
        let Ok(url) = rx.recv_timeout(Duration::from_secs(20)) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("process printed no `{ready_prefix}` line").into());
        };
        let server = Self { child, url };
        if let Some(path) = discovery {
            wait_until("discovery file created", Duration::from_secs(10), || {
                path.is_file()
            })?;
        }
        Ok(server)
    }

    pub fn terminate(&mut self) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let pid = i32::try_from(self.child.id())?;
        // SAFETY: `pid` is our unreaped child process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() > deadline {
                return Err("child did not exit after SIGTERM".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ForegroundServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Daemons(pub Vec<i32>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: test cleanup of daemon processes started by this test.
            unsafe { libc::kill(*pid, libc::SIGKILL) };
        }
    }
}
