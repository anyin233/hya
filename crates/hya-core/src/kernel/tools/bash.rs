//! Standard `bash` tool for `hya-core`.
//!
//! Executes shell commands in isolated process groups with
//! [`hya_tool::PermissionPlane`] (`Action::Bash`) authorization, run-budget
//! awareness, yielding for long-running commands, and coding output caps.
//! Per ADR-0026, `bash` is exempt from `ProjectScope` path boundaries.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, SeekFrom};
use std::os::unix::ffi::OsStringExt as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::path::display_path;
use hya_tool::process_group::{BudgetGroup, GroupOutcome};
use hya_tool::{
    Action, Resource, Tool, ToolCtx, ToolError, ToolResultPolicy, cap_tool_output_with_policy,
    deadline,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio::process::Command;

use super::{check_cancel, closed_obj_schema, workdir_normalized};

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
#[serde(deny_unknown_fields)]
struct Args {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "bash",
            "Run a shell command in the working directory with `bash -c` if Bash is on PATH, otherwise with `sh -c`. Returns combined stdout and stderr followed by one status line: `[exit code: N]`, `[still running: …]`, `[stopped: …]`, or `[not started: …]`. For output over 30,000 bytes, `output` shows the first 10,000 and last 20,000 bytes and names the file that holds all of it. `timeout` is how long this call waits. With a run time budget, one call waits at most a tenth of the budget. A command still running then is not stopped: the call returns what it has printed so far, and the command keeps running in the background, with its further output going to a file named in the result. Yielded commands are stopped when the run ends. Without a budget, a job started with `&` can outlive its shell; redirect its output to a file to keep it. If the end-of-budget cap ends the wait, Bash stops the command.",
            json!({
                "command": {"type": "string", "description": "Shell command"},
                "timeout": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Seconds this call waits for the command. Default 300. With a run time budget, one call waits at most a tenth of the budget. A command still running then keeps running in the background, unless the end-of-budget cap ends the wait."},
                "cwd": {"type": "string", "description": "Optional working directory relative to the session workdir or absolute"},
            }),
            &["command"],
        )
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        let Args {
            command,
            timeout,
            cwd: requested_cwd,
        } = super::args(input)?;
        if timeout.is_some_and(|seconds| !(1..=MAX_TIMEOUT_SECS).contains(&seconds)) {
            return Err(ToolError::Input(format!(
                "timeout must be between 1 and {MAX_TIMEOUT_SECS} seconds"
            )));
        }

        // Per ADR-0026, Bash is exempt from ProjectScope path boundaries;
        // authorization runs through `Action::Bash` with the full command.
        ctx.permission
            .assert(Action::Bash, Resource::Command(command.clone()))
            .await?;
        check_cancel(ctx)?;

        let cwd = match requested_cwd.as_deref() {
            Some(path) => super::resolve(&ctx.workdir, path),
            None => workdir_normalized(&ctx.workdir),
        };
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
            dir: Some(dir.clone()),
        };
        let path = dir.join("output");
        // One descriptor for both streams keeps their interleaving.
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)?;
        // The shared cap has a floor. A call cannot use more than the run has left.
        let deadline = deadline::current();
        let (limit, capped) = match deadline {
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
            let note = "The command was not started: the run's time budget has no time left.";
            return Ok(cap_tool_output_with_policy(
                json!({
                    "title": command,
                    "exit_code": null,
                    "output": with_status(String::new(), &format!("not started: {note}")),
                    "timed_out": note,
                    "metadata": {
                        "timedOut": true,
                        "cwd": display_path(&cwd),
                        "truncated": false,
                    },
                }),
                ToolResultPolicy::Coding,
            ));
        }
        // Creating the timer here starts the clock before spawning the shell.
        let timer = tokio::time::sleep(limit);
        let mut process = Command::new(SHELL.as_path());
        process
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file);
        let exit = dir.join("exit_code");
        let cancel = ctx.cancel.child_token();
        let abandoned = cancel.clone().drop_guard();
        let status =
            BudgetGroup::run(process, deadline, &cancel, timer, capped, exit.clone()).await?;
        let printed = std::fs::metadata(&path)?.len();
        let truncated = printed > MAX_OUTPUT_BYTES;
        let pgid = match status {
            GroupOutcome::Finished(status) => {
                let spilled = if truncated {
                    match spill_output_to_workdir(&path, &workdir_normalized(&ctx.workdir)) {
                        Ok(published) => Some(published),
                        Err(_) => {
                            job.dir = None;
                            Some(path.clone())
                        }
                    }
                } else {
                    None
                };
                let display_source = spilled.as_deref().unwrap_or(&path);
                let output = read_output(display_source, printed).await?;
                let code = json!(exit_code(status));
                let mut metadata = json!({
                    "exit": code,
                    "cwd": display_path(&cwd),
                    "truncated": truncated,
                });
                if let Some(spilled_path) = spilled {
                    metadata["outputPath"] = json!(display_path(&spilled_path));
                }
                return Ok(cap_tool_output_with_policy(
                    json!({
                        "title": command,
                        "exit_code": code,
                        "output": with_status(output, &format!("exit code: {code}")),
                        "metadata": metadata,
                    }),
                    ToolResultPolicy::Coding,
                ));
            }
            GroupOutcome::Stopped => {
                let spilled = if truncated {
                    match spill_output_to_workdir(&path, &workdir_normalized(&ctx.workdir)) {
                        Ok(published) => Some(published),
                        Err(_) => {
                            job.dir = None;
                            Some(path.clone())
                        }
                    }
                } else {
                    None
                };
                let display_source = spilled.as_deref().unwrap_or(&path);
                let output = read_output(display_source, printed).await?;
                let note = format!(
                    "The command was still running after {limit:?} (the run's time budget capped the timeout). Bash killed its process group."
                );
                let mut metadata = json!({
                    "timedOut": true,
                    "cwd": display_path(&cwd),
                    "truncated": truncated,
                });
                if let Some(spilled_path) = spilled {
                    metadata["outputPath"] = json!(display_path(&spilled_path));
                }
                return Ok(cap_tool_output_with_policy(
                    json!({
                        "title": command,
                        "exit_code": null,
                        "output": with_status(output, &format!("stopped: {note}")),
                        "timed_out": note,
                        "metadata": metadata,
                    }),
                    ToolResultPolicy::Coding,
                ));
            }
            GroupOutcome::Yielded(pgid) => pgid,
        };
        let output = read_output(&path, printed).await?;
        abandoned.disarm();
        job.dir = None;
        let reason = if limit < requested {
            "the run's time budget lets one call wait at most a tenth of it"
        } else if timeout.is_some() {
            "the `timeout` this call passed"
        } else {
            "the default; pass `timeout` in seconds to request a longer wait"
        };
        let (out, exit_quoted) = (quoted(&path), quoted(&exit));
        let note = format!(
            "This call waited {limit:?} ({reason}). The command was still running then and was left running in the background as process group {pgid}; it was not stopped. All its output, what is shown above and whatever it prints from now on, goes to {out}. Its exit code is written to {exit_quoted} when it finishes or the run stops it. To follow it, run for example `tail -n 20 {out}` and `cat {exit_quoted}`. To wait again, sleep briefly in another call before reading the output. To stop it if it is no longer needed, run `kill -- -{pgid}`."
        );
        Ok(cap_tool_output_with_policy(
            json!({
                "title": command,
                "exit_code": null,
                "output": with_status(output, &format!("still running: {note}")),
                "still_running": note,
                "metadata": {
                    "stillRunning": true,
                    "pgid": pgid,
                    "outputPath": display_path(&path),
                    "exitPath": display_path(&exit),
                    "cwd": display_path(&cwd),
                    "truncated": truncated,
                },
            }),
            ToolResultPolicy::Coding,
        ))
    }
}

fn with_status(mut output: String, status: &str) -> String {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push('[');
    output.push_str(status);
    output.push(']');
    output
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

/// Removes the temporary directory unless the result gives it to the caller.
struct Job {
    dir: Option<PathBuf>,
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            // Drop cannot report an error; a directory left behind is only stray temp files.
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

static NEXT_SPILL_ID: AtomicU64 = AtomicU64::new(0);

/// Atomically publish a finished/stopped truncated output file into
/// `<workdir>/.hya/tool-output/tool_<millis>_<pid>_<id>.txt` with mode `0o600`.
fn spill_output_to_workdir(source: &Path, workdir: &Path) -> io::Result<PathBuf> {
    let artifact_root = workdir.join(".hya/tool-output");
    std::fs::create_dir_all(&artifact_root)?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let id = NEXT_SPILL_ID.fetch_add(1, Ordering::Relaxed);
    let name = format!("tool_{millis}_{}_{id}.txt", std::process::id());
    let published = artifact_root.join(&name);
    let staged_path = artifact_root.join(format!(".{name}.tmp"));

    struct StagedGuard {
        path: PathBuf,
        armed: bool,
    }
    impl Drop for StagedGuard {
        fn drop(&mut self) {
            if self.armed {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut dest = options.open(&staged_path)?;
    let mut guard = StagedGuard {
        path: staged_path,
        armed: true,
    };
    let mut src = File::open(source)?;
    io::copy(&mut src, &mut dest)?;
    dest.sync_all()?;
    drop(dest);
    std::fs::rename(&guard.path, &published)?;
    guard.armed = false;
    Ok(published)
}
