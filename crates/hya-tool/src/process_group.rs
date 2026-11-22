//! Unix process groups for file-backed Bash calls.

use std::io::{self, PipeReader, Read as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

use crate::deadline::{self, RunDeadline};

/// A shell in its own process group. Budgeted calls keep the leader waitable
/// until the group finishes, so its PGID cannot be reused during supervision.
pub struct BudgetGroup {
    child: Child,
    pgid: u32,
    sentinel: Option<AsyncFd<PipeReader>>,
}

impl BudgetGroup {
    /// Start the shell. For an unbudgeted bare call, completion follows the leader alone.
    pub fn spawn(mut command: Command, wait_for_descendants: bool) -> io::Result<Self> {
        command.process_group(0);
        let (sentinel, writer) = if wait_for_descendants {
            let (reader, writer) = std::io::pipe()?;
            // SAFETY: the new pipe reader is read-only with no other status flags.
            if unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = writer.as_raw_fd();
            // Clear CLOEXEC only in this child. Concurrent spawns cannot inherit its writer.
            // SAFETY: fcntl is async-signal-safe and writer is held until after spawn.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            (Some(AsyncFd::new(reader)?), Some(writer))
        } else {
            (None, None)
        };
        let child = command.spawn()?;
        drop(writer);
        let pgid = child
            .id()
            .ok_or_else(|| io::Error::other("spawned Bash has no PID"))?;
        Ok(Self {
            child,
            pgid,
            sentinel,
        })
    }

    /// PID of the shell that leads this group.
    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    fn eof(mut reader: &PipeReader) -> io::Result<()> {
        let mut buf = [0; 1024];
        while reader.read(&mut buf)? != 0 {}
        Ok(())
    }

    fn exited(&self) -> io::Result<bool> {
        // SAFETY: waitid fills a zeroed siginfo_t; WNOWAIT keeps the leader's PID reserved.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.pgid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: si_pid is the child PID for this waitid result, or zero if still running.
        if unsafe { info.si_pid() } != self.pgid as libc::pid_t {
            return Ok(false);
        }
        #[cfg(target_os = "linux")]
        {
            // Descendants may close the inherited writer (e.g. subprocess.Popen).
            // Zombie descendants have exited even if their new parent has yet to reap them.
            let group = self.pgid.to_string();
            for entry in std::fs::read_dir("/proc")? {
                let entry = entry?;
                let name = entry.file_name();
                let pid = name.as_encoded_bytes();
                if !pid.iter().all(u8::is_ascii_digit) || pid == group.as_bytes() {
                    continue;
                }
                let stat = match std::fs::read(entry.path().join("stat")) {
                    Ok(stat) => stat,
                    Err(e)
                        if e.kind() == io::ErrorKind::NotFound
                            || e.raw_os_error() == Some(libc::ESRCH) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                // comm can contain spaces, closing parentheses and non-UTF8 bytes.
                if let Some(end) = stat.iter().rposition(|&b| b == b')') {
                    let tail = &stat[end + 1..];
                    let mut fields = tail
                        .split(|b| b.is_ascii_whitespace())
                        .filter(|s| !s.is_empty());
                    if !matches!(fields.next(), Some(b"Z" | b"X" | b"x"))
                        && fields.nth(1) == Some(group.as_bytes())
                    {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Wait for completion and reap the leader.
    pub async fn wait_for_group(&mut self) -> io::Result<ExitStatus> {
        if let Some(sentinel) = &self.sentinel {
            sentinel
                .async_io(tokio::io::Interest::READABLE, Self::eof)
                .await?;
            while !self.exited()? {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        }
        self.child.wait().await
    }

    /// Reap only if the entire supervised group is finished.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(sentinel) = &self.sentinel {
            match Self::eof(sentinel.get_ref()) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                result => result?,
            }
            if !self.exited()? {
                return Ok(None);
            }
        }
        self.child.try_wait()
    }

    fn kill(&self) -> io::Result<()> {
        if self.child.id().is_some() {
            // SAFETY: the unreaped child pins this process group ID.
            if unsafe { libc::killpg(self.pgid as libc::pid_t, libc::SIGKILL) } < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    /// Kill group members and reap the shell. A writer outside the group can stay open.
    pub async fn stop(&mut self) -> io::Result<()> {
        self.kill()?;
        while !self.exited()? {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        self.child.wait().await?;
        Ok(())
    }

    /// Register before returning the job to its caller. Exit status publication
    /// finishes before shutdown releases the registration.
    pub fn supervise(
        mut self,
        path: PathBuf,
        cancel: CancellationToken,
        deadline: Option<&RunDeadline>,
    ) {
        let (shutdown, guard) = deadline.map_or_else(
            deadline::register_unbudgeted_job,
            RunDeadline::register_background_job,
        );
        let end = deadline.map(|d| tokio::time::Instant::now() + d.remaining());
        tokio::spawn(async move {
            let _guard = guard;
            let result: io::Result<()> = async {
                let timeout = async {
                    match end {
                        Some(end) => tokio::time::sleep_until(end).await,
                        None => std::future::pending().await,
                    }
                };
                let status = tokio::select! {
                    biased;
                    () = cancel.cancelled() => self.try_wait()?,
                    () = shutdown.cancelled() => self.try_wait()?,
                    () = timeout => self.try_wait()?,
                    status = self.wait_for_group() => Some(status?),
                };
                let code = if let Some(status) = status {
                    status.code().or_else(|| status.signal().map(|s| 128 + s))
                } else {
                    self.stop().await?;
                    Some(128 + libc::SIGKILL)
                };
                let tmp = path.with_extension("tmp");
                tokio::fs::write(&tmp, format!("{}\n", serde_json::json!(code))).await?;
                tokio::fs::rename(tmp, path).await
            }
            .await;
            if let Err(error) = result {
                eprintln!("record Bash exit for group {}: {error}", self.pgid);
            }
        });
    }
}

impl Drop for BudgetGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
