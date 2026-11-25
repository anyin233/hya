use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};

use crate::closed_obj_schema;
use async_trait::async_trait;
use hya_proto::ToolSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use hya_tool::deadline::RunDeadline;
use hya_tool::path::{absolutize, display_path, normalize, resolve_file};
use hya_tool::{Action, Resource};
use hya_tool::{MAX_CODING_OUTPUT_PATH_BYTES, json_char_len, serialized_string_len};
use hya_tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};

const DEFAULT_TIMEOUT_SECONDS: f64 = 300.0;
const MIN_TIMEOUT_SECONDS: f64 = 1.0;
const MAX_TIMEOUT_SECONDS: f64 = 3600.0;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const PIPE_BUFFER_BYTES: usize = 8192;
const FINAL_DRAIN_BYTES: usize = 1024 * 1024;
const POST_KILL_DRAIN_GRACE: Duration = Duration::from_millis(100);
#[cfg(unix)]
const ORPHAN_PIPE_GRACE: Duration = Duration::from_millis(200);
#[cfg(unix)]
const POLL_INTERVAL_MILLISECONDS: libc::c_int = 50;

static NEXT_OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

/// Parsed Bash arguments after closed-schema validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellInput {
    command: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    timeout: Option<f64>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    pty: bool,
}

/// Validated deadline settings used by both command execution paths.
#[derive(Clone, Copy, Debug)]
struct TimeoutSettings {
    seconds: f64,
    duration: Option<Duration>,
    clamped: bool,
    budget_tenth_capped: bool,
    default_wait: bool,
    /// Whole seconds the run had left when its deadline shortened the timeout.
    deadline_left: Option<u64>,
}

impl TimeoutSettings {
    /// Shorten the timeout to what a budgeted run has left (see
    /// [`RunDeadline::cap`]); no timeout is capped too.
    fn capped_by(mut self, deadline: Option<&RunDeadline>) -> Self {
        let Some(deadline) = deadline else {
            return self;
        };
        let requested = self.duration;
        let tenth =
            (deadline.budget() / 10).max(Duration::from_secs(hya_tool::deadline::MIN_TOOL_SECS));
        if requested.is_none_or(|wait| wait > tenth) {
            self.duration = Some(tenth);
            self.seconds = tenth.as_secs_f64();
            self.budget_tenth_capped = true;
            self.clamped = false;
        }
        // Compare both caps against the original request so the end cap wins a tie.
        let (end, _) = deadline.cap(None);
        let left = deadline.remaining();
        let end = end.min(left);
        if self.duration.is_none_or(|wait| end <= wait) {
            self.duration = Some(end);
            self.seconds = end.as_secs_f64().round();
            self.deadline_left = Some(left.as_secs());
            self.budget_tenth_capped = false;
            self.clamped = false;
        }
        self
    }
}

/// Completion state retained for a structured Bash result.
#[derive(Debug)]
enum Completion {
    Finished(Option<i32>),
    TimedOut,
    Cancelled,
    #[cfg(unix)]
    NotStarted,
    #[cfg(unix)]
    StillRunning {
        pgid: u32,
        output_path: PathBuf,
        exit_path: PathBuf,
    },
}

/// Captured bytes and the armed owner of an optional complete artifact.
struct OutputCapture {
    inline: Vec<u8>,
    output_path: Option<PathBuf>,
    artifact_root: PathBuf,
    artifact_owner: Option<ArtifactOwner>,
    #[cfg(unix)]
    job: Option<Job>,
}

/// Own one staged Bash output artifact until a successful result publishes it.
///
/// The file is written under a private temporary name and atomically renamed
/// only after all raw bytes have been flushed and synchronized.  Keeping the
/// owner armed through result shaping removes both partial files and artifacts
/// that belong to cancelled or failed calls.
struct ArtifactOwner {
    file: Option<File>,
    path: PathBuf,
    published_path: PathBuf,
    armed: bool,
}

impl ArtifactOwner {
    /// Create a private staged artifact before its first byte is written.
    ///
    /// # Parameters
    /// - `artifact_root`: Existing or newly-created directory for artifacts.
    ///
    /// # Errors
    /// Returns an I/O error when the directory or exclusive mode-0600 file
    /// cannot be created.
    fn create(artifact_root: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(artifact_root).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "create Bash output artifact directory {}: {error}",
                    display_path(artifact_root)
                ),
            )
        })?;
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let id = NEXT_OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("tool_{millis}_{}_{id}.txt", std::process::id());
        let published_path = artifact_root.join(&name);
        let path = artifact_root.join(format!(".{name}.tmp"));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "create Bash output artifact {}: {error}",
                    display_path(&path)
                ),
            )
        })?;
        Ok(Self {
            file: Some(file),
            path,
            published_path,
            armed: true,
        })
    }

    /// Borrow the staged file while it is still owned by this guard.
    ///
    /// # Errors
    /// Returns an I/O error if the file was already closed for publication.
    fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("Bash output artifact is already closed"))
    }

    /// Flush, synchronize, and atomically publish the complete artifact.
    ///
    /// # Errors
    /// Returns an I/O error when flushing, synchronizing, or renaming fails.
    fn publish(&mut self) -> io::Result<()> {
        let file = self.file_mut()?;
        file.flush().map_err(|error| {
            io::Error::new(error.kind(), format!("flush Bash output artifact: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("synchronize Bash output artifact: {error}"),
            )
        })?;
        drop(self.file.take());
        std::fs::rename(&self.path, &self.published_path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "publish Bash output artifact {}: {error}",
                    display_path(&self.published_path)
                ),
            )
        })?;
        self.path.clone_from(&self.published_path);
        Ok(())
    }

    /// Disarm cleanup after the result has published its `outputPath`.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ArtifactOwner {
    fn drop(&mut self) {
        drop(self.file.take());
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl OutputCapture {
    /// Spill an inline-only capture when result notices consume its budget.
    ///
    /// # Errors
    /// Returns an I/O error when the complete raw stream cannot be published.
    fn spill_inline(&mut self) -> io::Result<()> {
        if self.artifact_owner.is_some() {
            return Ok(());
        }
        let mut owner = ArtifactOwner::create(&self.artifact_root)?;
        owner.file_mut()?.write_all(&self.inline)?;
        owner.publish()?;
        self.output_path = Some(owner.path.clone());
        self.artifact_owner = Some(owner);
        Ok(())
    }

    /// Disarm the artifact only after the successful result contains its path.
    fn disarm_artifact(&mut self) {
        if let Some(owner) = self.artifact_owner.as_mut() {
            owner.disarm();
        }
    }
}

/// Incremental output sink with a bounded preview and complete spill artifact.
///
/// The first output chunk that crosses the inline limit creates the artifact,
/// writes the preview already observed, and then streams all later bytes there.
/// This keeps result memory bounded while retaining cross-stream observation
/// order in the artifact.
struct OutputSink {
    artifact_root: PathBuf,
    inline: Vec<u8>,
    artifact: Option<ArtifactOwner>,
}

impl OutputSink {
    /// Create an empty sink rooted at the session's tool-output directory.
    fn new(artifact_root: PathBuf) -> Self {
        Self {
            artifact_root,
            inline: Vec::with_capacity(MAX_OUTPUT_BYTES),
            artifact: None,
        }
    }

    /// Append one observed chunk without retaining more than the inline bound.
    ///
    /// # Errors
    /// Returns an I/O error when the lazy artifact cannot be created or written.
    fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if let Some(artifact) = self.artifact.as_mut() {
            artifact.file_mut()?.write_all(bytes)?;
            return Ok(());
        }
        if bytes.len() <= MAX_OUTPUT_BYTES.saturating_sub(self.inline.len()) {
            self.inline.extend_from_slice(bytes);
            return Ok(());
        }

        let mut artifact = ArtifactOwner::create(&self.artifact_root)?;
        artifact.file_mut()?.write_all(&self.inline)?;
        artifact.file_mut()?.write_all(bytes)?;

        let remaining = MAX_OUTPUT_BYTES.saturating_sub(self.inline.len());
        self.inline.extend_from_slice(&bytes[..remaining]);
        self.artifact = Some(artifact);
        Ok(())
    }

    /// Flush and atomically publish the sink before its path enters a result.
    ///
    /// # Errors
    /// Returns an I/O error when the artifact cannot be flushed or synchronized.
    fn finish(mut self) -> io::Result<OutputCapture> {
        let (output_path, artifact_owner) = if let Some(mut artifact) = self.artifact.take() {
            artifact.publish()?;
            let path = artifact.path.clone();
            (Some(path), Some(artifact))
        } else {
            (None, None)
        };
        Ok(OutputCapture {
            inline: self.inline,
            output_path,
            artifact_root: self.artifact_root,
            artifact_owner,
            #[cfg(unix)]
            job: None,
        })
    }
}

/// Captured command state returned after all child output has been drained.
struct CapturedRun {
    completion: Completion,
    output: OutputCapture,
}

/// Canonical Bash command adapter.
pub(crate) struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    /// Return the canonical model-facing Bash name.
    fn name(&self) -> &str {
        "bash"
    }

    /// Return the closed Oh My Pi-compatible Bash input schema.
    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "bash",
            "Run a Bash command in the working directory.",
            json!({
                "command": { "type": "string" },
                "env": {
                    "type": "object",
                    "additionalProperties": { "type": "string" }
                },
                "timeout": {
                    "type": "number",
                    "minimum": 0,
                    "description": "Timeout in seconds. Defaults to 300 seconds; finite values are clamped to 1-3600 seconds, and 0 disables the requested timeout. Under a run time budget, waits are capped at budget/10 (minimum 5 seconds). On Unix a non-PTY command still running then yields stillRunning with live output and exit paths; the end-of-budget cap stops it."
                },
                "cwd": { "type": "string" },
                "pty": { "type": "boolean" }
            }),
            &["command"],
        )
    }

    /// Preserve the structured coding result envelope through persistence.
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    /// Validate, authorize, execute, and shape one Bash invocation.
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let ShellInput {
            command,
            env,
            timeout,
            cwd: requested_cwd,
            pty,
        } = serde_json::from_value(input)
            .map_err(|error| ToolError::Input(format!("invalid Bash input: {error}")))?;
        let timeout = timeout_settings(timeout)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        // Bash has no path boundary (ADR-0026): its cwd may be any directory
        // and only the command's invocation rules apply.
        ctx.permission
            .assert(Action::Bash, Resource::Command(command.clone()))
            .await?;
        let cwd = resolve_cwd(ctx, requested_cwd.as_deref());

        let started = Instant::now();
        let artifact_root = normalize(&absolutize(&ctx.workdir)).join(".hya/tool-output");
        let deadline = hya_tool::deadline::current();
        let timeout = timeout.capped_by(deadline);
        #[cfg(unix)]
        if !pty && let Some(deadline) = deadline {
            let captured = execute_budgeted_file(
                &command,
                env,
                &cwd,
                artifact_root,
                timeout,
                deadline,
                &ctx.cancel,
            )
            .await?;
            return shape_result(
                command,
                cwd,
                false,
                timeout,
                started.elapsed(),
                captured,
                &ctx.cancel,
            );
        }
        let captured = if pty {
            #[cfg(unix)]
            {
                execute_pty(
                    command.clone(),
                    env,
                    cwd.clone(),
                    artifact_root,
                    timeout,
                    deadline,
                    ctx.cancel.clone(),
                )
                .await?
            }
            #[cfg(not(unix))]
            {
                return Err(ToolError::Other(
                    "Bash PTY execution is unavailable on this platform".to_string(),
                ));
            }
        } else {
            execute_pipes(
                command.clone(),
                env,
                cwd.clone(),
                artifact_root,
                timeout,
                deadline,
                ctx.cancel.clone(),
            )
            .await?
        };

        if matches!(captured.completion, Completion::Cancelled) || ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        shape_result(
            command,
            cwd,
            pty,
            timeout,
            started.elapsed(),
            captured,
            &ctx.cancel,
        )
    }
}

/// Validate a timeout and convert it to the bounded execution settings.
///
/// # Errors
/// Returns [`ToolError::Input`] for negative or non-finite values.
fn timeout_settings(requested: Option<f64>) -> Result<TimeoutSettings, ToolError> {
    let Some(requested) = requested else {
        return Ok(TimeoutSettings {
            seconds: DEFAULT_TIMEOUT_SECONDS,
            duration: Some(Duration::from_secs(300)),
            clamped: false,
            budget_tenth_capped: false,
            default_wait: true,
            deadline_left: None,
        });
    };
    if !requested.is_finite() || requested < 0.0 {
        return Err(ToolError::Input(
            "timeout must be a finite non-negative number of seconds".to_string(),
        ));
    }
    if requested == 0.0 {
        return Ok(TimeoutSettings {
            seconds: 0.0,
            duration: None,
            clamped: false,
            budget_tenth_capped: false,
            default_wait: false,
            deadline_left: None,
        });
    }
    let seconds = requested.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);
    Ok(TimeoutSettings {
        seconds,
        duration: Some(Duration::from_secs_f64(seconds)),
        clamped: seconds != requested,
        budget_tenth_capped: false,
        default_wait: false,
        deadline_left: None,
    })
}

/// Resolve an optional cwd relative to the call's canonical workdir.
fn resolve_cwd(ctx: &ToolCtx, requested: Option<&str>) -> PathBuf {
    let base = normalize(&absolutize(&ctx.workdir));
    requested.map_or_else(|| base.clone(), |path| resolve_file(&base, path))
}

#[cfg(unix)]
struct Job {
    cancel: Option<CancellationToken>,
    dir: Option<PathBuf>,
}

#[cfg(unix)]
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(dir) = self.dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(unix)]
async fn execute_budgeted_file(
    command: &str,
    env: BTreeMap<String, String>,
    cwd: &Path,
    artifact_root: PathBuf,
    timeout: TimeoutSettings,
    deadline: &RunDeadline,
    cancel: &CancellationToken,
) -> Result<CapturedRun, ToolError> {
    use std::os::unix::ffi::OsStringExt as _;
    use std::process::Stdio;

    let mut sink = OutputSink::new(artifact_root);
    let mut template = std::env::temp_dir()
        .join("hya-bash-XXXXXX")
        .into_os_string()
        .into_vec();
    template.push(0);
    // SAFETY: writable NUL-terminated template with the six required trailing Xs.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    template.pop();
    let dir = PathBuf::from(std::ffi::OsString::from_vec(template));
    let mut job = Job {
        cancel: None,
        dir: Some(dir.clone()),
    };
    let path = dir.join("output");
    let file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&path)?;
    let left = deadline.remaining();
    if left.is_zero() {
        return Ok(CapturedRun {
            completion: Completion::NotStarted,
            output: sink.finish()?,
        });
    }
    let wait = timeout.duration.unwrap_or(left);
    let timer = tokio::time::sleep(wait.min(left));
    let mut process = tokio::process::Command::new("bash");
    process
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env("PYTHONUNBUFFERED", "1")
        .envs(env)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);
    use hya_tool::process_group::{BudgetGroup, GroupOutcome};
    let exit_path = dir.join("exit_code");
    let cancel = cancel.child_token();
    job.cancel = Some(cancel.clone());
    let completion = match BudgetGroup::run(
        process,
        Some(deadline),
        &cancel,
        timer,
        timeout.deadline_left.is_some() || left < wait,
        exit_path.clone(),
    )
    .await?
    {
        GroupOutcome::Finished(status) => Completion::Finished(status.code()),
        GroupOutcome::Stopped => Completion::TimedOut,
        GroupOutcome::Yielded(pgid) => Completion::StillRunning {
            pgid,
            output_path: path.clone(),
            exit_path,
        },
    };
    let mut file = File::open(&path)?;
    let output = if matches!(completion, Completion::StillRunning { .. }) {
        let mut output = sink.finish()?;
        let len = file.metadata()?.len();
        file.take(len.min(MAX_OUTPUT_BYTES as u64))
            .read_to_end(&mut output.inline)?;
        if len > MAX_OUTPUT_BYTES as u64 {
            output.output_path = Some(path);
        }
        output.job = Some(job);
        output
    } else {
        let mut buf = [0; PIPE_BUFFER_BYTES];
        loop {
            let count = file.read(&mut buf)?;
            if count == 0 {
                break;
            }
            sink.push(&buf[..count])?;
        }
        sink.finish()?
    };
    Ok(CapturedRun { completion, output })
}

/// Capture pipe output concurrently while waiting for the process.
///
/// The select loop reads whichever stream is observed ready first, preventing
/// one full pipe from blocking the child before the other stream is consumed.
///
/// # Errors
/// Returns a contextual tool error for read, wait, termination, or sink failures.
async fn execute_pipes(
    command: String,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    artifact_root: PathBuf,
    timeout: TimeoutSettings,
    deadline: Option<&RunDeadline>,
    cancel: CancellationToken,
) -> Result<CapturedRun, ToolError> {
    let mut process = tokio::process::Command::new("bash");
    process
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("PYTHONUNBUFFERED", "1")
        .envs(env);
    #[cfg(unix)]
    process.process_group(0);
    let (shutdown, registration) = hya_tool::deadline::register_job(deadline)?;
    if cancel.is_cancelled() || shutdown.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut child = process
        .spawn()
        .map_err(|error| contextual_io("spawn Bash process", error))?;
    let mut process_group = child.id();
    let worker_cancel = cancel.child_token();
    let _abandoned = worker_cancel.clone().drop_guard();
    let (reply, result) = oneshot::channel();
    tokio::spawn(async move {
        let _registration = registration;
        let mut sink = OutputSink::new(artifact_root);
        let captured = match capture_pipes(
            &mut child,
            &mut process_group,
            &mut sink,
            timeout.duration,
            &worker_cancel,
            &shutdown,
        )
        .await
        {
            Ok(completion) => sink
                .finish()
                .map(|output| CapturedRun { completion, output })
                .map_err(|error| contextual_io("finish Bash output capture", error)),
            Err(error) => {
                // Outside Linux the group poll may already have reaped the leader.
                if child.id().is_none() {
                    process_group = None;
                }
                let cleanup = terminate_pipe_child(&mut child, &mut process_group).await;
                Err(with_cleanup_error(error, cleanup))
            }
        };
        let _ = reply.send(captured);
    });
    result.await.map_err(io::Error::other)?
}

/// Drain both pipes after reaping, bounding capture of foreign-session writers.
///
/// # Errors
/// Returns contextual errors for pipe reads, child waits, or process cleanup.
async fn capture_pipes(
    child: &mut Child,
    process_group: &mut Option<u32>,
    sink: &mut OutputSink,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
    shutdown: &CancellationToken,
) -> Result<Completion, ToolError> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing Bash stdout pipe"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing Bash stderr pipe"))?;
    let mut deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
    let mut drain_cutoff = None;
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut reaped = false;
    let mut completion = None;
    let mut terminated = false;
    let mut stdout_buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut stderr_buffer = [0_u8; PIPE_BUFFER_BYTES];

    #[cfg(unix)]
    let mut group = hya_tool::process_group::ProcessGroup::new(
        process_group.ok_or_else(|| io::Error::other("spawned Bash has no PID"))?,
    );
    #[cfg(unix)]
    let mut poll = tokio::time::interval(Duration::from_millis(15));
    loop {
        if !terminated {
            let cancelled = cancel.is_cancelled() || shutdown.is_cancelled();
            if cancelled || deadline.is_some_and(|at| Instant::now() >= at) {
                terminate_pipe_child(child, process_group).await?;
                reaped = true;
                terminated = true;
                completion = Some(if cancelled {
                    Completion::Cancelled
                } else {
                    Completion::TimedOut
                });
                deadline = None;
                drain_cutoff = Some(Instant::now() + POST_KILL_DRAIN_GRACE);
            }
        }
        if reaped && stdout_done && stderr_done {
            break;
        }
        if drain_cutoff.is_some_and(|at| Instant::now() >= at) {
            drain_ready_pipe(&mut stdout, sink)
                .await
                .map_err(|error| contextual_io("drain Bash stdout", error))?;
            drain_ready_pipe(&mut stderr, sink)
                .await
                .map_err(|error| contextual_io("drain Bash stderr", error))?;
            break;
        }
        let wait = async {
            #[cfg(unix)]
            {
                poll.tick().await;
                if !group.finished(|| child.try_wait())? {
                    return Ok(None);
                }
            }
            child.wait().await.map(Some)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled(), if !terminated => {}
            _ = shutdown.cancelled(), if !terminated => {}
            _ = wait_for_deadline(deadline) => {}
            _ = wait_for_deadline(drain_cutoff) => {}
            result = wait, if !reaped => {
                if let Some(status) = result.map_err(|error| contextual_io("wait for Bash process group", error))? {
                    reaped = true;
                    *process_group = None;
                    completion = Some(Completion::Finished(status.code()));
                    #[cfg(unix)]
                    { drain_cutoff = Some(Instant::now() + ORPHAN_PIPE_GRACE); }
                }
            }
            result = stdout.read(&mut stdout_buffer), if !stdout_done => {
                let count = result.map_err(|error| contextual_io("read Bash stdout", error))?;
                stdout_done = count == 0;
                sink.push(&stdout_buffer[..count]).map_err(|error| contextual_io("capture Bash stdout", error))?;
            }
            result = stderr.read(&mut stderr_buffer), if !stderr_done => {
                let count = result.map_err(|error| contextual_io("read Bash stderr", error))?;
                stderr_done = count == 0;
                sink.push(&stderr_buffer[..count]).map_err(|error| contextual_io("capture Bash stderr", error))?;
            }
        }
    }
    Ok(completion.unwrap_or(Completion::Finished(None)))
}

/// Capture only immediately ready pipe bytes, bounded against foreign writers.
#[cfg(unix)]
async fn drain_ready_pipe(pipe: &mut impl AsRawFd, sink: &mut OutputSink) -> io::Result<()> {
    drain_ready_fd(pipe.as_raw_fd(), sink)
}

#[cfg(not(unix))]
async fn drain_ready_pipe(
    pipe: &mut (impl tokio::io::AsyncRead + Unpin),
    sink: &mut OutputSink,
) -> io::Result<()> {
    use std::task::Poll;

    let mut buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut remaining = FINAL_DRAIN_BYTES;
    while remaining > 0 {
        let limit = remaining.min(buffer.len());
        let result = {
            // A cooperative yield is not evidence that the pipe is empty.
            let mut read =
                std::pin::pin!(tokio::task::unconstrained(pipe.read(&mut buffer[..limit])));
            std::future::poll_fn(|cx| Poll::Ready(std::future::Future::poll(read.as_mut(), cx)))
                .await
        };
        match result {
            Poll::Ready(Ok(0)) | Poll::Pending => break,
            Poll::Ready(Ok(count)) => {
                sink.push(&buffer[..count])?;
                remaining -= count;
            }
            Poll::Ready(Err(error)) => return Err(error),
        }
    }
    Ok(())
}

/// Read nonblocking descriptors directly so reactor readiness cannot hide bytes.
#[cfg(unix)]
fn drain_ready_fd(fd: std::os::fd::RawFd, sink: &mut OutputSink) -> io::Result<()> {
    let mut buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut remaining = FINAL_DRAIN_BYTES;
    while remaining > 0 {
        let limit = remaining.min(buffer.len());
        // SAFETY: the capture loop owns this nonblocking pipe or PTY descriptor;
        // buffer is writable for at least limit bytes.
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), limit) };
        if count < 0 {
            let error = io::Error::last_os_error();
            // Linux PTY masters report EIO when their slave side closes.
            if error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::EIO)
            {
                break;
            }
            return Err(error);
        }
        if count == 0 {
            break;
        }
        let count = count as usize;
        sink.push(&buffer[..count])?;
        remaining -= count;
    }
    Ok(())
}

/// Wait for an absolute deadline, or remain pending when deadlines are disabled.
async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Terminate and reap a non-PTY process group.
///
/// # Errors
/// Returns a contextual tool error if the group cannot be terminated or reaped.
async fn terminate_pipe_child(
    child: &mut Child,
    process_group: &mut Option<u32>,
) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        terminate_pipe_group(
            child,
            process_group,
            hya_tool::process_group::finish_group_termination,
        )
        .await
    }
    #[cfg(not(unix))]
    {
        if process_group.is_some() {
            child
                .start_kill()
                .map_err(|error| contextual_io("terminate Bash process", error))?;
            *process_group = None;
        }
        reap_pipe_child(child).await
    }
}

async fn reap_pipe_child(child: &mut Child) -> Result<(), ToolError> {
    if child.id().is_some() {
        child
            .wait()
            .await
            .map_err(|error| contextual_io("reap Bash process", error))?;
    }
    Ok(())
}

#[cfg(unix)]
async fn terminate_pipe_group(
    child: &mut Child,
    process_group: &mut Option<u32>,
    finish_group: fn(u32) -> io::Result<()>,
) -> Result<(), ToolError> {
    let Some(pgid) = process_group.filter(|_| child.id().is_some()) else {
        *process_group = None;
        return reap_pipe_child(child).await;
    };
    hya_tool::process_group::kill_group(pgid)?;
    let quiet = tokio::task::spawn_blocking(move || finish_group(pgid))
        .await
        .map_err(io::Error::other)?;
    // Releasing the leader's PID ends ownership. Even a failed quiescence
    // check must leave the outer error handler with no group to signal.
    *process_group = None;
    reap_pipe_child(child).await?;
    quiet?;
    Ok(())
}

/// Execute a command attached to a real PTY on a blocking worker.
#[cfg(unix)]
async fn execute_pty(
    command: String,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    artifact_root: PathBuf,
    timeout: TimeoutSettings,
    deadline: Option<&RunDeadline>,
    cancel: CancellationToken,
) -> Result<CapturedRun, ToolError> {
    let (shutdown, registration) = hya_tool::deadline::register_job(deadline)?;
    if cancel.is_cancelled() || shutdown.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let worker_cancel = cancel.child_token();
    let _abandoned = worker_cancel.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        let _registration = registration;
        run_pty_blocking(
            command,
            env,
            cwd,
            artifact_root,
            timeout.duration,
            worker_cancel,
            shutdown,
        )
    })
    .await
    .map_err(|error| ToolError::Other(format!("Bash PTY worker failed: {error}")))?
}

/// Spawn and capture a PTY child while polling its nonblocking master.
#[cfg(unix)]
fn run_pty_blocking(
    command: String,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    artifact_root: PathBuf,
    timeout: Option<Duration>,
    cancel: CancellationToken,
    shutdown: CancellationToken,
) -> Result<CapturedRun, ToolError> {
    use pty_process::blocking::{Command, open};
    use std::os::fd::AsRawFd;

    let mut sink = OutputSink::new(artifact_root);
    let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
    let (mut master, pts) =
        open().map_err(|error| ToolError::Other(format!("Bash PTY setup failed: {error}")))?;
    set_nonblocking(master.as_raw_fd())
        .map_err(|error| contextual_io("set Bash PTY master nonblocking", error))?;
    if cancel.is_cancelled() || shutdown.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .env("PYTHONUNBUFFERED", "1")
        .envs(env)
        .spawn(pts)
        .map_err(|error| ToolError::Other(format!("spawn Bash PTY process: {error}")))?;
    let pgid = child.id();
    let mut group = hya_tool::process_group::ProcessGroup::new(pgid);

    let mut buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut pinned_group = Some(pgid);
    let mut drain_cutoff = None;
    let mut eof = false;
    let mut completion = None;
    loop {
        let cancelled = cancel.is_cancelled() || shutdown.is_cancelled();
        if cancelled || deadline.is_some_and(|at| Instant::now() >= at) {
            if completion.is_none() {
                terminate_pty_child(&mut child, &mut pinned_group)?;
                drain_cutoff = Some(Instant::now() + POST_KILL_DRAIN_GRACE);
            }
            completion = Some(if cancelled {
                Completion::Cancelled
            } else {
                Completion::TimedOut
            });
        } else if completion.is_none() {
            match group.finished(|| {
                let status = child.try_wait()?;
                if status.is_some() {
                    pinned_group = None;
                }
                Ok(status)
            }) {
                Ok(true) => {
                    pinned_group = None;
                    let status = child
                        .wait()
                        .map_err(|error| contextual_io("reap Bash PTY process", error))?;
                    completion = Some(Completion::Finished(status.code()));
                    drain_cutoff = Some(Instant::now() + ORPHAN_PIPE_GRACE);
                }
                Ok(false) => {}
                Err(error) => {
                    let wait_error = contextual_io("poll Bash PTY process group", error);
                    let cleanup = terminate_pty_child(&mut child, &mut pinned_group);
                    return Err(with_cleanup_error(wait_error, cleanup));
                }
            }
        }
        if completion.is_some() && eof {
            break;
        }
        if drain_cutoff.is_some_and(|at| Instant::now() >= at) {
            if let Err(error) = drain_ready_fd(master.as_raw_fd(), &mut sink) {
                let drain_error = contextual_io("drain Bash PTY output", error);
                let cleanup = terminate_pty_child(&mut child, &mut pinned_group);
                return Err(with_cleanup_error(drain_error, cleanup));
            }
            break;
        }
        if eof {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let mut pollfd = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        // SAFETY: pollfd points to one valid master descriptor owned by this worker.
        let polled = unsafe { libc::poll(&mut pollfd, 1, POLL_INTERVAL_MILLISECONDS) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            let poll_error = contextual_io("poll Bash PTY master", error);
            let cleanup = terminate_pty_child(&mut child, &mut pinned_group);
            return Err(with_cleanup_error(poll_error, cleanup));
        }
        if polled == 0 {
            continue;
        }
        match master.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(count) => {
                if let Err(error) = sink.push(&buffer[..count]) {
                    let capture_error = contextual_io("capture Bash PTY output", error);
                    let cleanup = terminate_pty_child(&mut child, &mut pinned_group);
                    return Err(with_cleanup_error(capture_error, cleanup));
                }
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                // Linux PTY masters report EIO when the slave side closes. The
                // child may not be waitable in the same scheduler tick.
                eof = true;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                let read_error = contextual_io("read Bash PTY master", error);
                let cleanup = terminate_pty_child(&mut child, &mut pinned_group);
                return Err(with_cleanup_error(read_error, cleanup));
            }
        }
    }
    // Close the master after EOF or the final drain grace.
    drop(master);
    let output = sink
        .finish()
        .map_err(|error| contextual_io("finish Bash PTY output", error))?;
    Ok(CapturedRun {
        completion: completion.unwrap_or(Completion::Finished(None)),
        output,
    })
}

/// Set a PTY master descriptor to nonblocking mode before polling it.
#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: fcntl operates on the descriptor owned by the PTY worker.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the flags came from F_GETFL for this same valid descriptor.
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Kill a PTY process group and reap its leader when it is still waitable.
#[cfg(unix)]
fn terminate_pty_child(
    child: &mut std::process::Child,
    process_group: &mut Option<u32>,
) -> Result<(), ToolError> {
    let Some(pgid) = *process_group else {
        return Ok(());
    };
    hya_tool::process_group::kill_group(pgid)?;
    let quiet = hya_tool::process_group::finish_group_termination(pgid);
    *process_group = None;
    child
        .wait()
        .map_err(|error| contextual_io("reap Bash PTY process", error))?;
    quiet?;
    Ok(())
}

/// Return whether any member remains in the saved process group.
#[cfg(all(unix, test))]
fn process_group_alive(process_group: Option<u32>) -> bool {
    let Some(process_group) = process_group else {
        return false;
    };
    let Ok(pid) = libc::pid_t::try_from(process_group) else {
        return false;
    };
    let Some(group) = pid.checked_neg() else {
        return false;
    };
    // SAFETY: kill with signal zero only probes the saved process group.
    let result = unsafe { libc::kill(group, 0) };
    result == 0 || (result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// Build the bounded `{title, output, metadata}` Bash result envelope.
///
/// The raw stream is spilled after notices are rendered when those notices
/// would otherwise consume part of the inline budget.  This keeps timeout and
/// clamp diagnostics visible while the artifact retains every raw byte.
///
/// # Errors
/// Returns a typed I/O error when a late spill cannot be published.
fn shape_result(
    command: String,
    cwd: PathBuf,
    pty: bool,
    timeout: TimeoutSettings,
    duration: Duration,
    mut captured: CapturedRun,
    cancel: &CancellationToken,
) -> Result<Value, ToolError> {
    let timed_out = matches!(captured.completion, Completion::TimedOut);
    let exit = match captured.completion {
        Completion::Finished(code) => code,
        _ => None,
    };
    let mut output_path = captured.output.output_path.clone();
    let mut displayed_output_path = output_path
        .as_deref()
        .map(validate_output_path)
        .transpose()?;
    let mut truncated = output_path.is_some();
    let signaled = matches!(captured.completion, Completion::Finished(None));
    let mut metadata = Map::new();
    let mut notice = String::new();
    #[cfg(unix)]
    match &captured.completion {
        Completion::StillRunning { pgid, output_path, exit_path } => {
            let out = validate_output_path(output_path)?;
            let exit = validate_output_path(exit_path)?;
            metadata.insert("stillRunning".into(), json!(true));
            metadata.insert("pgid".into(), json!(pgid));
            metadata.insert("outputPath".into(), json!(out));
            metadata.insert("exitPath".into(), json!(exit));
            let reason = if timeout.budget_tenth_capped { "budget/10" } else if timeout.default_wait { "default 300s" } else { "requested timeout" };
            let quote = |p: &str| format!("'{}'", p.replace('\'', r"'\''"));
            let (out, exit) = (quote(&out), quote(&exit));
            notice = format!("<shell_metadata>\nWaited {} seconds ({reason}). Process group {pgid} is still running. Read live output with tail -n 20 {out}, read its exit code with cat {exit}, or stop it with kill -- -{pgid}.\n</shell_metadata>\n\n", timeout.seconds);
        }
        Completion::NotStarted => notice = "<shell_metadata>\nCommand not started because the run budget expired.\n</shell_metadata>\n\n".into(),
        _ => {}
    }
    let prefix_for = |path: Option<&str>| {
        format!(
            "{notice}{}",
            shell_notice_prefix(timeout, path, timed_out, exit, signaled)
        )
    };
    let mut prefix = prefix_for(displayed_output_path.as_deref());
    if shell_output_len(&prefix, &captured.output.inline) > MAX_OUTPUT_BYTES
        && output_path.is_none()
    {
        #[cfg(unix)]
        if let Completion::StillRunning {
            output_path: path, ..
        } = &captured.completion
        {
            output_path = Some(path.clone());
        }
        if output_path.is_none() {
            captured
                .output
                .spill_inline()
                .map_err(|error| contextual_io("spill Bash output capture", error))?;
            output_path = captured.output.output_path.clone();
        }
        displayed_output_path = output_path
            .as_deref()
            .map(validate_output_path)
            .transpose()?;
        truncated = true;
        prefix = prefix_for(displayed_output_path.as_deref());
    }
    let output = render_shell_output(
        &prefix,
        &captured.output.inline,
        displayed_output_path.as_deref(),
    );

    #[cfg(unix)]
    let timed_out = timed_out || matches!(captured.completion, Completion::NotStarted);
    metadata.insert("exit".to_string(), json!(exit));
    metadata.insert("timedOut".to_string(), json!(timed_out));
    metadata.insert("truncated".to_string(), json!(truncated));
    metadata.insert(
        "durationMs".to_string(),
        json!(duration.as_millis().try_into().unwrap_or(u64::MAX)),
    );
    metadata.insert("timeoutSeconds".to_string(), json!(timeout.seconds));
    metadata.insert("pty".to_string(), json!(pty));
    metadata.insert("cwd".to_string(), json!(display_path(&cwd)));
    if timeout.deadline_left.is_some() {
        metadata.insert("deadlineCapped".to_string(), json!(true));
    }
    if timeout.clamped {
        metadata.insert("timeoutClamped".to_string(), json!(true));
    }
    if let Some(path) = displayed_output_path {
        metadata.insert("outputPath".to_string(), json!(path));
    }
    let result = json!({
        "title": command,
        "output": output,
        "metadata": metadata,
    });
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    #[cfg(unix)]
    if let Some(job) = &mut captured.output.job {
        job.cancel = None;
        job.dir = None;
    }
    captured.output.disarm_artifact();
    Ok(result)
}

/// Render and validate an artifact path before it can enter notices or metadata.
///
/// # Parameters
/// - `path`: Published, cleanup-owned artifact path.
///
/// # Returns
/// The display path when its complete JSON string fits the atomic path budget.
///
/// # Errors
/// Returns a contextual I/O error when the complete path cannot fit the result
/// contract.
fn validate_output_path(path: &Path) -> Result<String, ToolError> {
    let displayed = display_path(path);
    if serialized_string_len(&displayed) > MAX_CODING_OUTPUT_PATH_BYTES {
        return Err(contextual_io(
            "publish Bash output artifact path",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "complete output path exceeds the result metadata budget",
            ),
        ));
    }
    Ok(displayed)
}

/// Build timeout, spill, and completion notices before the inline decision.
fn shell_notice_prefix(
    timeout: TimeoutSettings,
    output_path: Option<&str>,
    timed_out: bool,
    exit: Option<i32>,
    signaled: bool,
) -> String {
    let mut prefix = String::new();
    if let Some(left) = timeout.deadline_left {
        prefix.push_str(&format!(
            "<shell_metadata>\nTimeout shortened to {:.0} seconds: the run ends in {left} seconds.\n</shell_metadata>\n\n",
            timeout.seconds
        ));
    }
    if timeout.clamped {
        prefix.push_str(&format!(
            "<shell_metadata>\nRequested timeout was clamped to {:.0} seconds.\n</shell_metadata>\n\n",
            timeout.seconds
        ));
    }
    if let Some(path) = output_path {
        prefix.push_str(&format!(
            "...output truncated...\n\nFull output saved to: {path}\n\n"
        ));
    }
    if let Some(code) = exit.filter(|code| *code != 0) {
        prefix.push_str(&format!(
            "<shell_metadata>\nCommand exited with code {code}.\n</shell_metadata>\n\n"
        ));
    }
    if signaled {
        prefix.push_str("<shell_metadata>\nCommand terminated by signal.\n</shell_metadata>\n\n");
    }
    if timed_out {
        prefix.push_str(&format!(
            "<shell_metadata>\nBash timeout terminated the command after {:.0} seconds.\n</shell_metadata>\n\n",
            timeout.seconds
        ));
    }
    prefix
}

/// Measure the notice plus complete lossy preview as one serialized JSON string.
fn shell_output_len(prefix: &str, inline: &[u8]) -> usize {
    let inline_len = if inline.is_empty() {
        serialized_string_len("(no output)").saturating_sub(2)
    } else {
        serialized_string_len(&String::from_utf8_lossy(inline)).saturating_sub(2)
    };
    serialized_string_len(prefix).saturating_add(inline_len)
}

/// Render a notice followed by a UTF-8-safe preview, retaining both ends when spilled.
fn render_shell_output(prefix: &str, inline: &[u8], output_path: Option<&str>) -> String {
    let mut output = prefix.to_owned();
    if inline.is_empty() {
        output.push_str("(no output)");
        return output;
    }
    let available = MAX_OUTPUT_BYTES.saturating_sub(serialized_string_len(&output));
    let mut head = String::from_utf8_lossy(inline).into_owned();
    let Some(path) = output_path else {
        truncate_utf8_for_json(&mut head, available);
        output.push_str(&head);
        return output;
    };
    let total_bytes = std::fs::metadata(path)
        .map(|metadata| metadata.len().try_into().unwrap_or(usize::MAX))
        .unwrap_or(inline.len());
    let mut tail = String::from_utf8_lossy(&read_artifact_tail(path, available.saturating_mul(4)))
        .into_owned();
    let mut marker = String::new();
    let mut head_budget;
    let mut tail_budget;
    for _ in 0..2 {
        let content_budget = available.saturating_sub(serialized_string_len(&marker));
        head_budget = content_budget / 5;
        tail_budget = content_budget.saturating_sub(head_budget);
        truncate_utf8_for_json(&mut head, head_budget);
        truncate_utf8_tail_for_json(&mut tail, tail_budget);
        let omitted = total_bytes.saturating_sub(head.len().saturating_add(tail.len()));
        marker = format!("[… {omitted} bytes omitted; full output: {path}]\n");
    }
    output.push_str(&head);
    output.push_str(&marker);
    output.push_str(&tail);
    output
}

fn read_artifact_tail(path: &str, max_bytes: usize) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(end) = file.seek(std::io::SeekFrom::End(0)) else {
        return Vec::new();
    };
    let size = (end as usize).min(max_bytes);
    if file.seek(std::io::SeekFrom::End(-(size as i64))).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::with_capacity(size);
    let _ = file.take(size as u64).read_to_end(&mut bytes);
    bytes
}

fn truncate_utf8_tail_for_json(text: &mut String, max_encoded_bytes: usize) {
    let mut used = 0usize;
    let mut start = text.len();
    for (offset, character) in text.char_indices().rev() {
        let encoded = json_char_len(character);
        if used.saturating_add(encoded) > max_encoded_bytes {
            break;
        }
        used += encoded;
        start = offset;
    }
    if start > 0 {
        text.drain(..start);
    }
}

/// Truncate a UTF-8 string by its JSON-encoded content bytes.
fn truncate_utf8_for_json(text: &mut String, max_encoded_bytes: usize) {
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, character) in text.char_indices() {
        let encoded = json_char_len(character);
        if used.saturating_add(encoded) > max_encoded_bytes {
            break;
        }
        used += encoded;
        end = offset + character.len_utf8();
    }
    if end < text.len() {
        text.truncate(end);
    }
}

/// Preserve a primary capture failure and attach any process-cleanup failure.
///
/// # Parameters
/// - `primary`: Error that caused capture to stop.
/// - `cleanup`: Result of terminating and reaping the owned process group.
///
/// # Returns
/// The primary error when cleanup succeeded, or one contextual error containing
/// both failures when cleanup could not establish the lifecycle invariant.
fn with_cleanup_error(primary: ToolError, cleanup: Result<(), ToolError>) -> ToolError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => ToolError::Other(format!(
            "{primary}; Bash process cleanup also failed: {cleanup}"
        )),
    }
}

/// Convert an I/O failure into a typed error with operation context.
fn contextual_io(operation: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Io(io::Error::other(format!("{operation}: {error}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_quiescence_reaps_and_releases_the_pipe_group() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut group = child.id();
        let error = terminate_pipe_group(&mut child, &mut group, |pgid| {
            // SAFETY: WNOWAIT checks the worker still owns the waitable child.
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            assert_eq!(
                unsafe {
                    libc::waitid(
                        libc::P_PID,
                        pgid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOWAIT,
                    )
                },
                0
            );
            Err(io::Error::other("injected quiescence failure"))
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected quiescence failure"));
        assert_eq!(child.id(), None);
        assert_eq!(group, None);
        terminate_pipe_group(&mut child, &mut group, |_| panic!("released group"))
            .await
            .unwrap();
    }

    #[test]
    fn spilled_output_preview_keeps_unique_tail_marker() {
        let path = std::env::temp_dir().join(format!("hya-shell-test-{}.txt", std::process::id()));
        let mut file = File::create(&path).expect("create test artifact");
        let tail_marker = "UNIQUE_FAILURE_SUMMARY_MARKER";
        let full = format!("{}{}\n", "head\n".repeat(MAX_OUTPUT_BYTES), tail_marker);
        file.write_all(full.as_bytes())
            .expect("write test artifact");

        let output = render_shell_output(
            "Full output saved to: artifact://test\n",
            b"head\nhead\n",
            Some(path.to_str().expect("UTF-8 test path")),
        );
        std::fs::remove_file(path).expect("remove test artifact");

        assert!(
            output.contains(tail_marker),
            "output did not retain tail: {output:?}"
        );
        assert!(output.contains("omitted"));
    }

    #[test]
    fn deadline_capped_timeout_is_explained() {
        let timeout = TimeoutSettings {
            seconds: 155.0,
            duration: Some(Duration::from_secs(155)),
            clamped: false,
            budget_tenth_capped: false,
            default_wait: false,
            deadline_left: Some(200),
        };
        let prefix = shell_notice_prefix(timeout, None, true, None, false);
        assert!(prefix.starts_with(
            "<shell_metadata>\nTimeout shortened to 155 seconds: the run ends in 200 seconds."
        ));
        assert!(prefix.contains("terminated the command after 155 seconds"));
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_five_seconds_ties_end_cap() -> Result<(), ToolError> {
        let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(8));
        let settings = timeout_settings(Some(5.0))?.capped_by(Some(&deadline));
        assert_eq!(settings.deadline_left, Some(8));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn budgeted_shell_waits_for_background_output() -> Result<(), ToolError> {
        let cwd = Path::new(".");
        let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(60));
        let run = execute_budgeted_file(
            "(sleep 0.2; echo bg-ok) &",
            BTreeMap::new(),
            cwd,
            cwd.into(),
            timeout_settings(Some(2.0))?,
            &deadline,
            &CancellationToken::new(),
        )
        .await?;
        assert!(matches!(run.completion, Completion::Finished(Some(0))));
        assert_eq!(run.output.inline, b"bg-ok\n");
        let run = execute_budgeted_file(
            "sleep .1 & exit 7",
            BTreeMap::new(),
            cwd,
            cwd.into(),
            timeout_settings(Some(1.0))?,
            &deadline,
            &CancellationToken::new(),
        )
        .await?;
        assert!(matches!(run.completion, Completion::Finished(Some(7))));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn budgeted_shell_finishes_while_a_detached_writer_is_alive() -> Result<(), ToolError> {
        let cwd = Path::new(".");
        let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(180));
        let started = Instant::now();
        let run = execute_budgeted_file(
            "python3 -c 'import os,time; os.setsid(); time.sleep(30)' & echo $!",
            BTreeMap::new(),
            cwd,
            cwd.into(),
            timeout_settings(Some(10.0))?,
            &deadline,
            &CancellationToken::new(),
        )
        .await?;
        let pid = String::from_utf8_lossy(&run.output.inline)
            .trim()
            .parse::<libc::pid_t>()
            .map_err(io::Error::other)?;
        assert!(pid > 0);
        // SAFETY: query and stop the specific background child printed by Bash.
        let session = unsafe { libc::getsid(pid) };
        let killed = unsafe { libc::kill(pid, libc::SIGKILL) };
        assert_eq!(session, pid);
        assert_eq!(killed, 0);
        assert!(matches!(run.completion, Completion::Finished(Some(0))));
        assert!(started.elapsed() < Duration::from_secs(5));
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn budgeted_shell_tracks_descendants_that_close_inherited_fds() -> Result<(), ToolError> {
        let cwd = Path::new(".");
        for (seconds, cancel_job) in [("0.25", false), ("60", true), ("60", false)] {
            let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(180));
            let cancel = CancellationToken::new();
            let command = format!(
                r#"python3 -c 'import subprocess; print(subprocess.Popen(["sleep", "{seconds}"]).pid)'"#
            );
            let timeout = timeout_settings(Some(1.0))?;
            let run = execute_budgeted_file(
                &command,
                BTreeMap::new(),
                cwd,
                cwd.into(),
                timeout,
                &deadline,
                &cancel,
            )
            .await?;
            let pid = String::from_utf8_lossy(&run.output.inline)
                .trim()
                .parse::<u32>()
                .map_err(io::Error::other)?;
            if seconds == "60" {
                let result = shape_result(
                    command,
                    cwd.into(),
                    false,
                    timeout,
                    Duration::ZERO,
                    run,
                    &cancel,
                )?;
                let meta = &result["metadata"];
                assert_eq!(meta["stillRunning"], true);
                assert!(Path::new(&format!("/proc/{pid}")).exists());
                let exit = Path::new(
                    meta["exitPath"]
                        .as_str()
                        .ok_or(io::Error::other("missing exitPath"))?,
                );
                if cancel_job {
                    cancel.cancel();
                    tokio::time::timeout(Duration::from_secs(3), async {
                        while !exit.exists() {
                            tokio::time::sleep(Duration::from_millis(15)).await;
                        }
                    })
                    .await
                    .map_err(io::Error::other)?;
                }
                deadline.shutdown_background_jobs().await;
                assert_eq!(std::fs::read_to_string(exit)?, "137\n");
                std::fs::remove_dir_all(
                    exit.parent()
                        .ok_or(io::Error::other("missing job directory"))?,
                )?;
            } else {
                assert!(matches!(run.completion, Completion::Finished(Some(0))));
            }
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(stat) => assert!(stat.rsplit_once(')').is_some_and(|(_, fields)| matches!(
                    fields.split_whitespace().next(),
                    Some("Z" | "X" | "x")
                ))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn yielded_groups_stop_on_cancel_or_deadline() -> Result<(), ToolError> {
        for (command, trigger) in [
            ("sleep 60", "cancel"),
            ("echo started; (sleep 60) &", "deadline"),
            ("sleep 60", "shutdown"),
        ] {
            let cancel = CancellationToken::new();
            let cwd = Path::new(".");
            let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(2));
            let mut timeout = timeout_settings(Some(1.0))?;
            timeout.duration = Some(Duration::from_millis(100));
            let run = execute_budgeted_file(
                command,
                BTreeMap::new(),
                cwd,
                cwd.into(),
                timeout,
                &deadline,
                &cancel,
            )
            .await?;
            let result = shape_result(
                command.into(),
                cwd.into(),
                false,
                timeout,
                Duration::ZERO,
                run,
                &cancel,
            )?;
            let meta = &result["metadata"];
            assert_eq!(meta["stillRunning"], true);
            let pgid = meta["pgid"]
                .as_u64()
                .ok_or(io::Error::other("missing pgid"))? as u32;
            assert!(process_group_alive(Some(pgid)));
            let path = Path::new(
                meta["exitPath"]
                    .as_str()
                    .ok_or(io::Error::other("missing exitPath"))?,
            );
            let output = std::fs::read_to_string(
                meta["outputPath"]
                    .as_str()
                    .ok_or(io::Error::other("missing outputPath"))?,
            )?;
            if trigger == "shutdown" {
                deadline.shutdown_background_jobs().await;
                assert!(path.is_file());
                assert!(!process_group_alive(Some(pgid)));
            } else if trigger == "cancel" {
                cancel.cancel();
            } else {
                assert_eq!(output, "started\n");
            }
            tokio::time::timeout(Duration::from_secs(4), async {
                while !path.exists() {
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
            })
            .await
            .map_err(|e| contextual_io("wait for group termination", e))?;
            assert_eq!(std::fs::read_to_string(path)?.trim(), "137");
            std::fs::remove_dir_all(path.parent().ok_or(io::Error::other("missing job dir"))?)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completed_budgeted_spill_is_an_owned_artifact() -> Result<(), ToolError> {
        let root = std::env::temp_dir().join(format!("hya-budget-spill-{}", std::process::id()));
        let deadline = RunDeadline::new(tokio::time::Instant::now(), Duration::from_secs(60));
        let run = execute_budgeted_file(
            "head -c 60000 /dev/zero",
            BTreeMap::new(),
            Path::new("."),
            root.clone(),
            timeout_settings(None)?,
            &deadline,
            &CancellationToken::new(),
        )
        .await?;
        assert!(matches!(run.completion, Completion::Finished(Some(0))));
        let path = run
            .output
            .output_path
            .as_ref()
            .ok_or(io::Error::other("missing spill"))?;
        assert_eq!(path.parent(), Some(root.as_path()));
        assert_eq!(std::fs::read(path)?, vec![0; 60000]);
        drop(run);
        std::fs::remove_dir(root)?;
        Ok(())
    }

    #[test]
    fn completion_notice_only_reports_nonzero_exit() {
        let timeout = timeout_settings(None).expect("default timeout");
        let failed = shell_notice_prefix(timeout, None, false, Some(101), false);
        let succeeded = shell_notice_prefix(timeout, None, false, Some(0), false);

        assert!(failed.contains("Command exited with code 101."));
        assert!(!succeeded.contains("Command exited"));
    }

    #[test]
    fn timeout_schema_describes_seconds_and_bounds() {
        let schema = ShellTool.schema();
        let timeout = &schema.input_schema["properties"]["timeout"]["description"];
        let description = timeout.as_str().expect("timeout description");
        assert!(description.contains("seconds"));
        assert!(description.contains("1-3600"));
        assert!(description.contains("0 disables"));
    }
}
