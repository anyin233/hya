//! `hya proxy`: readiness line, the `404 hya relay` body on a non-relay
//! path, and a clean exit on SIGTERM (docs/relay.md, docs/cli.md).

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn proxy(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .args(["proxy", "--host", "127.0.0.1", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A running `hya proxy`: killed on drop.
struct Proxy {
    child: Child,
    url: String,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `hya proxy` and wait for its readiness line.
fn start(root: &Path) -> Result<Proxy, Box<dyn std::error::Error>> {
    let mut child = proxy(root).spawn()?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("hya proxy listening on ") {
                let _ = tx.send(url.trim().to_string());
            }
        }
    });
    // Drain stderr so the child never blocks on a full pipe.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    }
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(url) => Ok(Proxy { child, url }),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("hya proxy printed no readiness line".into())
        }
    }
}

fn terminate(proxy: &mut Proxy) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let pid = i32::try_from(proxy.child.id())?;
    // SAFETY: `kill` has no memory-safety preconditions; `pid` is our own
    // child and has not been reaped yet.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = proxy.child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() > deadline {
            return Err("hya proxy did not exit after SIGTERM".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test]
async fn proxy_prints_readiness_line_serves_404_and_exits_cleanly_on_sigterm() -> TestResult {
    let root = scratch("hya-proxy-cli")?;
    let mut server = start(&root)?;

    // Readiness line: `hya proxy listening on http://<addr>` (no TLS, no
    // prefix in this invocation).
    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "unexpected readiness line: {}",
        server.url
    );

    // A non-relay path answers 404 with the `hya relay` body, so `hya relay
    // doctor` can recognize the proxy behind an intermediary.
    let response = reqwest::get(&server.url).await?;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    let body = response.text().await?;
    assert_eq!(body, "hya relay");

    // Clean exit on SIGTERM.
    let status = terminate(&mut server)?;
    assert!(status.success(), "{status:?}");
    Ok(())
}
