//! Spawn and supervise a real backend `serve` subprocess (`opencode` or `yaca`).
//!
//! Both backends print `<name> server listening on http://127.0.0.1:<port>` to their output,
//! which we parse for the base URL (see [`parse_listen_url`]). `Drop` guarantees no orphaned
//! server even if the caller forgets to shut down (PLAN.md R1/S-8).

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::error::{Result, SdkError};

const DEFAULT_BIN: &str = "opencode";
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// yaca binds its listener only after connecting MCP servers, which can be slow; give the
/// background connect a generous window before declaring the spawn dead.
const YACA_READY_TIMEOUT: Duration = Duration::from_secs(180);

/// `opencode serve` flags: ephemeral port on loopback, logs to stdout so the URL is parseable.
const OPENCODE_SERVE_ARGS: &[&str] = &[
    "serve",
    "--port",
    "0",
    "--hostname",
    "127.0.0.1",
    "--print-logs",
];
/// `yaca serve` flags: ephemeral loopback port (`:0`). yaca prints the same `listening on` line.
const YACA_SERVE_ARGS: &[&str] = &["serve", "--bind", "127.0.0.1:0"];

/// A running `opencode serve` process plus its discovered base URL.
#[derive(Debug)]
pub struct ServerHandle {
    child: Child,
    base_url: String,
    directory: String,
}

impl ServerHandle {
    /// Spawn `opencode serve` for `directory` on a random port and wait until it is ready.
    ///
    /// # Errors
    /// [`SdkError::Spawn`] if the process cannot start, [`SdkError::Readiness`] on timeout,
    /// [`SdkError::ListenUrlParse`] if the server exits before announcing a URL.
    pub async fn spawn(directory: &str) -> Result<Self> {
        Self::spawn_with(DEFAULT_BIN, directory, DEFAULT_READY_TIMEOUT).await
    }

    /// Spawn `opencode serve` with an explicit binary name and readiness timeout (used by tests
    /// to inject failures).
    ///
    /// # Errors
    /// See [`ServerHandle::spawn`].
    pub async fn spawn_with(bin: &str, directory: &str, timeout: Duration) -> Result<Self> {
        Self::spawn_args(bin, OPENCODE_SERVE_ARGS, directory, timeout).await
    }

    /// Spawn `yaca serve` on an ephemeral loopback port and wait until it announces its URL.
    ///
    /// # Errors
    /// See [`ServerHandle::spawn`].
    pub async fn spawn_yaca(bin: &str, directory: &str) -> Result<Self> {
        Self::spawn_args(bin, YACA_SERVE_ARGS, directory, YACA_READY_TIMEOUT).await
    }

    async fn spawn_args(
        bin: &str,
        args: &[&str],
        directory: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            // If the background connect is cancelled before readiness, the half-built handle is
            // dropped before `ServerHandle::drop` exists; kill_on_drop stops the child leaking.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SdkError::Spawn(e.to_string()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SdkError::Spawn("no stdout pipe".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SdkError::Spawn("no stderr pipe".into()))?;
        let (tx, mut rx) = mpsc::channel::<String>(256);
        spawn_line_reader(stdout, tx.clone());
        spawn_line_reader(stderr, tx);

        let found = tokio::time::timeout(timeout, async {
            while let Some(line) = rx.recv().await {
                if let Some(url) = parse_listen_url(&line) {
                    return Some(url);
                }
            }
            None
        })
        .await
        .map_err(|_| SdkError::Readiness(timeout))?;

        let Some(base_url) = found else {
            let _ = child.start_kill();
            return Err(SdkError::ListenUrlParse);
        };

        Ok(Self {
            child,
            base_url,
            directory: directory.to_string(),
        })
    }

    /// The server's base URL, e.g. `http://127.0.0.1:NNNNN`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The directory this server was scoped to.
    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // The child is its own process-group leader (`process_group(0)`), so signalling the
        // NEGATIVE pid reaches the whole tree — `opencode serve` spawns subprocesses that a
        // single-pid kill would orphan. Graceful SIGTERM, ~1s wait, then SIGKILL the group.
        if let Some(pid) = self.child.id() {
            let pid = pid as libc::pid_t;
            unsafe {
                libc::kill(-pid, libc::SIGTERM);
            }
            let mut exited = false;
            for _ in 0..50 {
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        exited = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            if !exited {
                let _ = self.child.start_kill();
            }
            let _ = self.child.try_wait();
        }
        // The real server runs in its own session and survives the group kill once the launcher
        // exits; tokio may also have already reaped the launcher (child.id() == None). So this
        // ALWAYS runs: kill whatever still holds the port.
        if let Some(port) = self
            .base_url
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<u16>().ok())
        {
            kill_port_listeners(port);
        }
    }
}

fn kill_port_listeners(port: u16) {
    let inodes = listen_socket_inodes(port);
    if inodes.is_empty() {
        return;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if inodes
                .iter()
                .any(|inode| target == format!("socket:[{inode}]"))
            {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                break;
            }
        }
    }
}

fn listen_socket_inodes(port: u16) -> Vec<String> {
    let mut inodes = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let columns: Vec<&str> = line.split_whitespace().collect();
            let (Some(local), Some(state), Some(inode)) =
                (columns.get(1), columns.get(3), columns.get(9))
            else {
                continue;
            };
            let Some((_, port_hex)) = local.rsplit_once(':') else {
                continue;
            };
            if *state == "0A"
                && u16::from_str_radix(port_hex, 16).is_ok_and(|listen_port| listen_port == port)
            {
                inodes.push((*inode).to_owned());
            }
        }
    }
    inodes
}

fn spawn_line_reader<R>(reader: R, tx: mpsc::Sender<String>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });
}

fn parse_listen_url(line: &str) -> Option<String> {
    let rest = line.split_once("listening on ")?.1;
    let url = rest.split_whitespace().next()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verified_listen_line() {
        let line = "opencode server listening on http://127.0.0.1:4096";
        assert_eq!(
            parse_listen_url(line).as_deref(),
            Some("http://127.0.0.1:4096")
        );
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(parse_listen_url("timestamp=... level=INFO message=loading").is_none());
    }

    #[tokio::test]
    async fn missing_binary_is_typed_spawn_error_not_a_hang() {
        let err = ServerHandle::spawn_with(
            "opencode-definitely-not-a-real-binary-xyz",
            ".",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, SdkError::Spawn(_)),
            "expected Spawn error, got {err:?}"
        );
    }
}
