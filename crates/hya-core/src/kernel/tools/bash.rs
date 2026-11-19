use std::ffi::OsString;
use std::io::{self, SeekFrom};
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError, deadline};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::process::{Child, Command};

/// Seconds a call waits for its command when it names no `timeout`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Longest `timeout` a call may name: far below the range in which a timer deadline overflows.
const MAX_TIMEOUT_SECS: u64 = 86_400;

/// Most bytes of a command's output one result carries: a result is sent again with every later
/// request. A longer output is shown as its head and its tail; the end gets the larger share
/// because results and errors are printed last.
const MAX_OUTPUT_BYTES: u64 = 30_000;
const HEAD_OUTPUT_BYTES: u64 = 10_000;
const TAIL_OUTPUT_BYTES: u64 = MAX_OUTPUT_BYTES - HEAD_OUTPUT_BYTES;

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
            "Run a shell command in the working directory with `bash -c` if Bash is on PATH, otherwise with `sh -c`. Returns its exit code and combined stdout and stderr. For output over 30,000 bytes, `output` shows the first 10,000 and last 20,000 bytes and names the file that holds all of it. `timeout` is how long this call waits. With a run time budget, one call waits at most a tenth of the budget. A command still running then is not stopped: the call returns what it has printed so far, and the command keeps running in the background, with its further output going to a file named in the result. A job the command starts with `&` also keeps running after the call returns; redirect its output to a file to keep it. If the end-of-budget cap ends the wait, Bash stops the command.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command"},
                    "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Seconds this call waits for the command. Default 300. With a run time budget, one call waits at most a tenth of the budget. A command still running then keeps running in the background, unless the end-of-budget cap ends the wait."},
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
        // A private directory per call: a fixed name in a shared temp root could be pre-created by
        // another user. It is created before the budget check so that check covers everything after it.
        let mut template = std::env::temp_dir()
            .join("hya-bash-XXXXXX")
            .into_os_string()
            .into_vec();
        template.push(0);
        // SAFETY: the template is a writable NUL-terminated path ending in XXXXXX, as mkdtemp requires.
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        template.pop();
        let dir = PathBuf::from(OsString::from_vec(template));
        let mut job = Job {
            pgid: None,
            dir: Some(dir.clone()),
        };
        let path = dir.join("output");
        // One descriptor for both streams keeps their interleaving.
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)?;
        // The shared cap has a floor. A call cannot use more than the run has left.
        let (limit, capped) = match deadline::current() {
            Some(deadline) => {
                let wait = requested.min(deadline.budget() / 10);
                let (cap, capped) = deadline.cap(Some(wait));
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
        let mut process = Command::new(SHELL.as_path());
        process
            .arg("-c")
            .arg(&command)
            .current_dir(&ctx.workdir)
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .process_group(0);
        let mut child = process.spawn()?;
        let pgid = child
            .id()
            .ok_or_else(|| io::Error::other("the spawned shell has no process id"))?;
        job.pgid = Some(pgid);
        let status = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            () = &mut timer => child.try_wait()?,
            status = child.wait() => Some(status?),
        };
        // What `output` reports is what the file held when the wait ended.
        let printed = std::fs::metadata(&path)?.len();
        if status.is_none() && capped {
            // Kill before reading: processes left in the group could keep writing.
            job.kill();
        }
        let output = read_output(&path, printed).await?;
        if printed > MAX_OUTPUT_BYTES {
            job.dir = None;
        }
        if let Some(status) = status {
            // A job the command started with `&` keeps running, as after any shell exits.
            job.pgid = None;
            return Ok(json!({
                "exit_code": exit_code(status),
                "output": output,
            }));
        }
        if capped {
            return Ok(json!({
                "exit_code": null,
                "output": output,
                "timed_out": format!(
                    "The command was still running after {limit:?} (the run's time budget capped the timeout). Bash killed its process group and returned the captured `output`."
                ),
            }));
        }
        let exit = dir.join("exit_code");
        tokio::spawn(record_exit(child, exit.clone()));
        // Disarmed only here, with no await before the return, so a dropped call kills the command
        // rather than leave a job nobody was told about.
        job.pgid = None;
        job.dir = None;
        let reason = if limit < requested {
            "the run's time budget lets one call wait at most a tenth of it"
        } else if timeout.is_some() {
            "the `timeout` this call passed"
        } else {
            "the default; pass `timeout` in seconds to request a longer wait"
        };
        let (out, exit) = (quoted(&path), quoted(&exit));
        Ok(json!({
            "exit_code": null,
            "output": output,
            "still_running": format!(
                "This call waited {limit:?} ({reason}). The command was still running then and was left running in the background as process group {pgid}; it was not stopped. All its output, what `output` shows and whatever it prints from now on, goes to {out}. If it ends while this run is still going, its exit code is written to {exit}. To follow it, run for example `tail -n 20 {out}` and `cat {exit}`. To wait again, sleep briefly in another call before reading the output. To stop it if it is no longer needed, run `kill -- -{pgid}`."
            ),
        }))
    }
}

/// The first `len` bytes the command printed: a file that keeps growing is read only that far.
/// Over [`MAX_OUTPUT_BYTES`], their head and tail around a marker that names the file.
async fn read_output(path: &Path, len: u64) -> io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    if len <= MAX_OUTPUT_BYTES {
        file.take(len).read_to_end(&mut bytes).await?;
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }

    (&mut file)
        .take(HEAD_OUTPUT_BYTES)
        .read_to_end(&mut bytes)
        .await?;
    let head = String::from_utf8_lossy(&bytes).into_owned();
    bytes.clear();
    file.seek(SeekFrom::Start(len - TAIL_OUTPUT_BYTES)).await?;
    file.take(TAIL_OUTPUT_BYTES).read_to_end(&mut bytes).await?;
    let tail = String::from_utf8_lossy(&bytes);
    let omitted = len - MAX_OUTPUT_BYTES;
    let path = quoted(path);
    Ok(format!(
        "{head}\n[… {omitted} bytes omitted here. All {len} bytes are in {path}. Search it with `grep`, or print a part with `sed -n` or `tail -c`.]\n{tail}"
    ))
}

/// Waits for a command this call left running and writes its exit code to `exit`.
async fn record_exit(mut child: Child, exit: PathBuf) {
    let partial = exit.with_extension("tmp");
    let written = async {
        let status = child.wait().await?;
        tokio::fs::write(&partial, format!("{}\n", json!(exit_code(status)))).await?;
        // The rename makes the file appear complete or not at all.
        tokio::fs::rename(&partial, &exit).await
    };
    if let Err(error) = written.await {
        tracing::warn!(exit = %exit.display(), "exit code of a background command not recorded: {error}");
    }
}

/// `path` single-quoted for a shell command line.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Exit code, or `128 + signal` for a command killed by a signal.
fn exit_code(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
}

/// The command's process group and the call's directory: dropping it kills the group and removes
/// the directory with its files.
struct Job {
    pgid: Option<u32>,
    dir: Option<PathBuf>,
}

impl Job {
    /// Signals the group at most once: once the group is gone its id can be reused.
    fn kill(&mut self) {
        if let Some(pgid) = self
            .pgid
            .take()
            .and_then(|pid| libc::pid_t::try_from(pid).ok())
        {
            // SAFETY: killpg only sends a signal; a group already gone yields ESRCH.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.kill();
        if let Some(dir) = &self.dir {
            // Drop cannot report an error; a directory left behind is only stray temp files.
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
