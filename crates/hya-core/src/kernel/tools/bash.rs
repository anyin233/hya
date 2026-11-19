use std::io::{self, Read as _};
use std::os::fd::AsRawFd as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError, deadline};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::pipe::Receiver;
use tokio::process::Command;

/// Seconds a command may run when the call names no `timeout`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Longest `timeout` a call may name: far below the range in which a timer deadline overflows.
const MAX_TIMEOUT_SECS: u64 = 86_400;

/// How long to wait for more output after the command exits or is killed.
const DRAIN: Duration = Duration::from_secs(1);

/// `bash` when one is on `PATH`, else `sh`; resolved once per process.
static SHELL: LazyLock<PathBuf> = LazyLock::new(|| {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("bash"))
                .find(|bash| bash.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("sh"))
});

pub(super) struct Bash;

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout: Option<u64>,
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "bash",
            "Run a shell command in the working directory with `bash -c` if Bash is on PATH, otherwise with `sh -c`. Bash kills processes still in the command's process group when the call returns, so a job that must outlive the call has to leave the group (for example with `setsid`). Returns its exit code and combined stdout and stderr. At `timeout`, Bash kills the command's process group and returns the output it captured.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command"},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Seconds the command may run before it is killed. Default 300."},
                },
                "required": ["command"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { command, timeout } = super::args(input)?;
        if timeout.is_some_and(|seconds| !(1..=MAX_TIMEOUT_SECS).contains(&seconds)) {
            return Err(ToolError::Input(format!(
                "timeout must be between 1 and {MAX_TIMEOUT_SECS} seconds"
            )));
        }
        let requested = Duration::from_secs(timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
        // The shared cap has a floor. A call cannot use more than the run has left.
        let run_deadline = deadline::current();
        let (limit, capped) = match run_deadline {
            Some(deadline) => {
                let (cap, capped) = deadline.cap(Some(requested));
                let left = deadline.remaining();
                (cap.min(left), capped || left < cap)
            }
            None => (requested, false),
        };
        if limit.is_zero() {
            // A command started now could still change files after the budget ended.
            return Ok(json!({
                "exit_code": null,
                "output": "",
                "timed_out": "The command was not started: the run's time budget has no time left.",
            }));
        }
        // Creating the timer here starts the clock before spawning the shell.
        let timer = tokio::time::sleep(limit);
        tokio::pin!(timer);
        // One pipe for both streams keeps their interleaving.
        let (reader, writer) = std::io::pipe()?;
        let mut reader = Receiver::from_owned_fd(reader.into())?;
        let mut process = Command::new(SHELL.as_path());
        process
            .arg("-c")
            .arg(&command)
            .current_dir(&ctx.workdir)
            .stdin(Stdio::null())
            .stdout(writer.try_clone()?)
            .stderr(writer)
            .process_group(0)
            .kill_on_drop(true);
        let mut child = process.spawn()?;
        // The command holds the pipe's write ends; drop them so EOF arrives.
        drop(process);
        let group = Group(child.id());
        let mut output = Vec::new();
        let (status, drain_expired) = {
            let capture = async {
                while reader.read_buf(&mut output).await? != 0 {}
                Ok::<(), io::Error>(())
            };
            tokio::pin!(capture);
            let mut eof = false;
            let status = loop {
                tokio::select! {
                    biased;
                    () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                    () = &mut timer => break child.try_wait()?,
                    status = child.wait() => break Some(status?),
                    result = &mut capture, if !eof => {
                        result?;
                        eof = true;
                    }
                }
            };
            // Background jobs in the group can otherwise keep the pipe open.
            drop(group);
            let drain = run_deadline.map_or(DRAIN, |deadline| DRAIN.min(deadline.remaining()));
            let drain_expired = if eof {
                false
            } else {
                tokio::select! {
                    biased;
                    () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                    () = tokio::time::sleep(drain) => true,
                    result = &mut capture => {
                        result?;
                        false
                    }
                }
            };
            (status, drain_expired)
        };
        if drain_expired {
            let reader = std::fs::File::from(reader.into_nonblocking_fd()?);
            let mut available: libc::c_int = 0;
            // SAFETY: FIONREAD writes the unread byte count to this valid int.
            let result =
                unsafe { libc::ioctl(reader.as_raw_fd(), libc::FIONREAD, &raw mut available) };
            if result == -1 {
                return Err(io::Error::last_os_error().into());
            }
            // Read the snapshot even if Tokio has not registered readiness.
            // A writer outside the group can keep adding bytes after this.
            reader
                .take(u64::from(available.cast_unsigned()))
                .read_to_end(&mut output)?;
        }
        let output = String::from_utf8_lossy(&output);
        let Some(status) = status else {
            let reason = if capped {
                "the run's time budget capped the timeout"
            } else if timeout.is_some() {
                "the `timeout` this call passed"
            } else {
                "the default; pass `timeout` in seconds for a command that needs longer"
            };
            return Ok(json!({
                "exit_code": null,
                "output": output,
                "timed_out": format!(
                    "The command was still running after {limit:?} ({reason}). Bash killed its process group and returned the captured `output`."
                ),
            }));
        };
        Ok(json!({
            "exit_code": exit_code(status),
            "output": output,
        }))
    }
}

/// Exit code, or `128 + signal` for a command killed by a signal.
fn exit_code(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
}

/// The command's process group, killed when dropped.
struct Group(Option<u32>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pgid) = self.0.and_then(|pid| libc::pid_t::try_from(pid).ok()) {
            // SAFETY: killpg only sends a signal; a group already gone yields ESRCH.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}
