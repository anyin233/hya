//! Unix process groups for file-backed Bash calls.

use std::io::{self, PipeReader, Read as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::ToolError;
use crate::deadline::{self, RunDeadline};

const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Result of the foreground wait for a file-backed shell.
pub enum GroupOutcome {
    /// The shell completed within the foreground wait.
    Finished(ExitStatus),
    /// The run budget stopped the command.
    Stopped,
    /// The supervisor still owns this process group and its exit file.
    Yielded(u32),
}

/// A shell in its own process group. Budgeted calls keep the leader waitable
/// until the group finishes, so its PGID cannot be reused during supervision.
pub struct BudgetGroup {
    child: Child,
    pgid: u32,
    sentinel: Option<AsyncFd<PipeReader>>,
    #[cfg(target_os = "linux")]
    quiet_scan: Option<(Vec<(u32, u64)>, tokio::time::Instant)>,
}

impl BudgetGroup {
    /// Start the shell. For an unbudgeted bare call, completion follows the leader alone.
    fn spawn(mut command: Command, wait_for_descendants: bool) -> io::Result<Self> {
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
            #[cfg(target_os = "linux")]
            quiet_scan: None,
        })
    }

    /// Admit before spawning. The same task owns the registration and shell
    /// from foreground execution through a possible yield and exit-file rename.
    pub async fn run(
        command: Command,
        deadline: Option<&RunDeadline>,
        cancel: &CancellationToken,
        timer: tokio::time::Sleep,
        stop_on_timeout: bool,
        exit: PathBuf,
    ) -> Result<GroupOutcome, ToolError> {
        let (shutdown, registration) = deadline::register_job(deadline)?;
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut group = Self::spawn(command, deadline.is_some())?;
        let end = deadline.map(|d| tokio::time::Instant::now() + d.remaining());
        let cancel = cancel.child_token();
        let abandoned = cancel.clone().drop_guard();
        let (reply, result) = oneshot::channel();
        tokio::spawn(async move {
            let _registration = registration;
            let foreground = async {
                let status = tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        group.stop().await?;
                        return Err(ToolError::Cancelled);
                    }
                    () = shutdown.cancelled() => {
                        group.stop().await?;
                        return Err(ToolError::Cancelled);
                    }
                    status = group.wait_for_group() => Some(status?),
                    () = timer => group.try_wait()?,
                };
                if let Some(status) = status {
                    Ok(GroupOutcome::Finished(status))
                } else if stop_on_timeout {
                    group.stop().await?;
                    Ok(GroupOutcome::Stopped)
                } else {
                    Ok(GroupOutcome::Yielded(group.pgid))
                }
            }
            .await;
            let yielded = matches!(foreground, Ok(GroupOutcome::Yielded(_)));
            let _ = reply.send(foreground);
            if yielded && let Err(error) = group.supervise(exit, cancel, shutdown, end).await {
                eprintln!("record Bash exit for group {}: {error}", group.pgid);
            }
        });
        let outcome = result.await.map_err(io::Error::other)?;
        abandoned.disarm();
        outcome
    }

    fn eof(mut reader: &PipeReader) -> io::Result<()> {
        let mut buf = [0; 1024];
        while reader.read(&mut buf)? != 0 {}
        Ok(())
    }

    fn exited(&mut self) -> io::Result<bool> {
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
            // A fork can move the last live member past a directory iterator.
            // Keep the leader waitable until two polls agree on the identities left behind.
            let Some(zombies) = group_zombies(std::path::Path::new("/proc"), self.pgid)? else {
                self.quiet_scan = None;
                return Ok(false);
            };
            if let Some((previous, since)) = &self.quiet_scan
                && previous == &zombies
            {
                return Ok(since.elapsed() >= POLL_INTERVAL);
            }
            self.quiet_scan = Some((zombies, tokio::time::Instant::now()));
            Ok(false)
        }
        #[cfg(not(target_os = "linux"))]
        Ok(true)
    }

    /// Wait for completion and reap the leader.
    async fn wait_for_group(&mut self) -> io::Result<ExitStatus> {
        if let Some(sentinel) = &self.sentinel {
            sentinel
                .async_io(tokio::io::Interest::READABLE, Self::eof)
                .await?;
            while !self.exited()? {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        self.child.wait().await
    }

    /// Reap only if the entire supervised group is finished.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
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
    async fn stop(&mut self) -> io::Result<()> {
        loop {
            self.kill()?;
            if self.exited()? {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        self.child.wait().await?;
        Ok(())
    }

    async fn supervise(
        &mut self,
        path: PathBuf,
        cancel: CancellationToken,
        shutdown: CancellationToken,
        end: Option<tokio::time::Instant>,
    ) -> io::Result<()> {
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
}

impl Drop for BudgetGroup {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

/// Return the zombie process identities, or None if any member could still do work.
#[cfg(target_os = "linux")]
fn group_zombies(proc_root: &std::path::Path, pgid: u32) -> io::Result<Option<Vec<(u32, u64)>>> {
    let mut zombies = Vec::new();
    for entry in std::fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let path = entry.path();
        let Some(leader) = proc_stat(&path)? else {
            continue;
        };
        if leader.group != pgid {
            continue;
        }
        if !leader.dead {
            return Ok(None);
        }

        // A process can still have live threads after its main thread exits.
        let Some(tasks) = still_present(std::fs::read_dir(path.join("task")))? else {
            continue;
        };
        for task in tasks {
            if let Some(stat) = proc_stat(&task?.path())?
                && !stat.dead
            {
                return Ok(None);
            }
        }
        zombies.push((pid, leader.start));
    }
    zombies.sort_unstable();
    Ok(Some(zombies))
}

#[cfg(target_os = "linux")]
struct ProcStat {
    dead: bool,
    group: u32,
    start: u64,
}

#[cfg(target_os = "linux")]
fn proc_stat(path: &std::path::Path) -> io::Result<Option<ProcStat>> {
    let Some(stat) = still_present(std::fs::read(path.join("stat")))? else {
        return Ok(None);
    };
    // comm may contain spaces, closing parentheses, and non-UTF8 bytes.
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid stat in {}", path.display()),
        )
    };
    let end = stat.iter().rposition(|&b| b == b')').ok_or_else(invalid)?;
    let mut fields = std::str::from_utf8(&stat[end + 1..])
        .map_err(|_| invalid())?
        .split_ascii_whitespace();
    let dead = matches!(fields.next(), Some("Z" | "X" | "x")); // field 3
    let group = fields
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)?; // field 5
    let start = fields
        .nth(16)
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)?; // field 22
    Ok(Some(ProcStat { dead, group, start }))
}

#[cfg(target_os = "linux")]
fn still_present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn zombie_process_is_quiet_only_after_its_last_thread_exits() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("group-scan-{}", hya_proto::SessionId::new()));
        let member = root.join("42");
        let worker = member.join("task/43");
        fs::create_dir_all(&worker)?;
        let stat = |state| {
            format!(
                "42 (name ) with spaces) {state} 1 10 {} 999 0",
                "0 ".repeat(16)
            )
        };
        fs::write(member.join("stat"), stat('Z'))?;
        fs::write(worker.join("stat"), stat('S'))?;
        assert_eq!(group_zombies(&root, 10)?, None);
        fs::write(worker.join("stat"), stat('X'))?;
        assert_eq!(group_zombies(&root, 10)?, Some(vec![(42, 999)]));
        fs::remove_dir_all(root)
    }
}
