use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio_util::sync::CancellationToken;

use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::output_cap::{MAX_CODING_OUTPUT_PATH_BYTES, json_char_len, serialized_string_len};
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};

const DEFAULT_TIMEOUT_SECONDS: f64 = 300.0;
const MIN_TIMEOUT_SECONDS: f64 = 1.0;
const MAX_TIMEOUT_SECONDS: f64 = 3600.0;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const CONTROL_RUNNING: u8 = 0;
const CONTROL_CANCELLED: u8 = 1;
const CONTROL_TIMED_OUT: u8 = 2;
const PIPE_BUFFER_BYTES: usize = 8192;
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
}

/// Completion state retained for a structured Bash result.
#[derive(Clone, Copy, Debug)]
enum Completion {
    Finished(Option<i32>),
    TimedOut,
    Cancelled,
}

/// Captured bytes and the armed owner of an optional complete artifact.
struct OutputCapture {
    inline: Vec<u8>,
    output_path: Option<PathBuf>,
    artifact_root: PathBuf,
    artifact_owner: Option<ArtifactOwner>,
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
        ToolSchema {
            name: ToolName::new("bash"),
            description: "Run a Bash command in the working directory.".to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "command": { "type": "string" },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "timeout": { "type": "number", "minimum": 0 },
                    "cwd": { "type": "string" },
                    "pty": { "type": "boolean" }
                },
                "required": ["command"]
            }),
            output_schema: None,
        }
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

        // The command subject is checked before resolving or authorizing cwd so
        // a denied command cannot probe an external directory.
        ctx.permission
            .assert(Action::Bash, Resource::Command(command.clone()))
            .await?;
        let cwd = resolve_cwd(ctx, requested_cwd.as_deref());
        assert_external_workdir(ctx, &cwd).await?;

        let started = Instant::now();
        let artifact_root = normalize(&absolutize(&ctx.workdir)).join(".hya/tool-output");
        let captured = if pty {
            #[cfg(unix)]
            {
                execute_pty(
                    command.clone(),
                    env,
                    cwd.clone(),
                    artifact_root,
                    timeout,
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
        });
    }
    let seconds = requested.clamp(MIN_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS);
    Ok(TimeoutSettings {
        seconds,
        duration: Some(Duration::from_secs_f64(seconds)),
        clamped: seconds != requested,
    })
}

/// Resolve an optional cwd relative to the call's canonical workdir.
fn resolve_cwd(ctx: &ToolCtx, requested: Option<&str>) -> PathBuf {
    let base = normalize(&absolutize(&ctx.workdir));
    requested.map_or_else(|| base.clone(), |path| resolve_file(&base, path))
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
    cancel: CancellationToken,
) -> Result<CapturedRun, ToolError> {
    let mut process = tokio::process::Command::new("bash");
    process
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .envs(env);
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process
        .spawn()
        .map_err(|error| contextual_io("spawn Bash process", error))?;
    let process_group = child.id();
    let mut stdout = child.stdout.take().ok_or_else(|| {
        contextual_io(
            "capture Bash stdout",
            io::Error::other("stdout pipe was not attached"),
        )
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        contextual_io(
            "capture Bash stderr",
            io::Error::other("stderr pipe was not attached"),
        )
    })?;
    let mut sink = OutputSink::new(artifact_root);
    let completion = match capture_pipes(
        &mut child,
        process_group,
        &mut stdout,
        &mut stderr,
        &mut sink,
        timeout.duration,
        &cancel,
    )
    .await
    {
        Ok(completion) => completion,
        Err(error) => {
            let cleanup = terminate_pipe_child(&mut child, process_group).await;
            return Err(with_cleanup_error(error, cleanup));
        }
    };
    let output = sink
        .finish()
        .map_err(|error| contextual_io("finish Bash output capture", error))?;
    Ok(CapturedRun { completion, output })
}

/// Drain both pipes until EOF after the process is reaped.
///
/// # Errors
/// Returns contextual errors for pipe reads, child waits, or process cleanup.
async fn capture_pipes(
    child: &mut Child,
    process_group: Option<u32>,
    stdout: &mut ChildStdout,
    stderr: &mut ChildStderr,
    sink: &mut OutputSink,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<Completion, ToolError> {
    let mut deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut reaped = false;
    let mut completion = None;
    let mut cancellation_seen = false;
    let mut stdout_buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut stderr_buffer = [0_u8; PIPE_BUFFER_BYTES];

    loop {
        if reaped && stdout_done && stderr_done {
            break;
        }
        if !reaped {
            if cancel.is_cancelled() {
                terminate_pipe_child(child, process_group).await?;
                reaped = true;
                completion = Some(Completion::Cancelled);
                cancellation_seen = true;
                deadline = None;
                continue;
            }
            if deadline.is_some_and(|at| Instant::now() >= at) {
                terminate_pipe_child(child, process_group).await?;
                reaped = true;
                completion = Some(Completion::TimedOut);
                deadline = None;
                continue;
            }
        } else {
            if !cancellation_seen && cancel.is_cancelled() {
                terminate_pipe_process_group(process_group)?;
                completion = Some(Completion::Cancelled);
                cancellation_seen = true;
                deadline = None;
            }
            if deadline.is_some_and(|at| Instant::now() >= at) {
                terminate_pipe_process_group(process_group)?;
                completion = Some(Completion::TimedOut);
                deadline = None;
            }
        }

        if !reaped {
            tokio::select! {
                _ = cancel.cancelled(), if !cancellation_seen => {
                    terminate_pipe_child(child, process_group).await?;
                    reaped = true;
                    completion = Some(Completion::Cancelled);
                    cancellation_seen = true;
                    deadline = None;
                }
                _ = wait_for_deadline(deadline) => {
                    terminate_pipe_child(child, process_group).await?;
                    reaped = true;
                    completion = Some(Completion::TimedOut);
                    deadline = None;
                }
                result = child.wait() => {
                    let status = result.map_err(|error| contextual_io("wait for Bash process", error))?;
                    reaped = true;
                    completion = Some(Completion::Finished(status.code()));
                }
                result = stdout.read(&mut stdout_buffer), if !stdout_done => {
                    let count = result.map_err(|error| contextual_io("read Bash stdout", error))?;
                    if count == 0 {
                        stdout_done = true;
                    } else {
                        sink.push(&stdout_buffer[..count])
                            .map_err(|error| contextual_io("capture Bash stdout", error))?;
                    }
                }
                result = stderr.read(&mut stderr_buffer), if !stderr_done => {
                    let count = result.map_err(|error| contextual_io("read Bash stderr", error))?;
                    if count == 0 {
                        stderr_done = true;
                    } else {
                        sink.push(&stderr_buffer[..count])
                            .map_err(|error| contextual_io("capture Bash stderr", error))?;
                    }
                }
            }
        } else {
            tokio::select! {
                _ = cancel.cancelled(), if !cancellation_seen => {
                    terminate_pipe_process_group(process_group)?;
                    completion = Some(Completion::Cancelled);
                    cancellation_seen = true;
                    deadline = None;
                }
                _ = wait_for_deadline(deadline) => {
                    terminate_pipe_process_group(process_group)?;
                    completion = Some(Completion::TimedOut);
                    deadline = None;
                }
                result = stdout.read(&mut stdout_buffer), if !stdout_done => {
                    let count = result.map_err(|error| contextual_io("read Bash stdout", error))?;
                    if count == 0 {
                        stdout_done = true;
                    } else {
                        sink.push(&stdout_buffer[..count])
                            .map_err(|error| contextual_io("capture Bash stdout", error))?;
                    }
                }
                result = stderr.read(&mut stderr_buffer), if !stderr_done => {
                    let count = result.map_err(|error| contextual_io("read Bash stderr", error))?;
                    if count == 0 {
                        stderr_done = true;
                    } else {
                        sink.push(&stderr_buffer[..count])
                            .map_err(|error| contextual_io("capture Bash stderr", error))?;
                    }
                }
            }
        }
    }
    Ok(completion.unwrap_or(Completion::Finished(None)))
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
    process_group: Option<u32>,
) -> Result<(), ToolError> {
    terminate_pipe_process_group(process_group)?;
    #[cfg(not(unix))]
    child
        .kill()
        .await
        .map_err(|error| contextual_io("terminate Bash process", error))?;
    child
        .wait()
        .await
        .map(|_| ())
        .map_err(|error| contextual_io("reap Bash process", error))
}

/// Terminate a non-PTY process group even after its direct shell was reaped.
///
/// # Errors
/// Returns a contextual tool error when the process group cannot be signalled.
fn terminate_pipe_process_group(process_group: Option<u32>) -> Result<(), ToolError> {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        let pid = libc::pid_t::try_from(pid).map_err(|_| {
            contextual_io(
                "terminate Bash process group",
                io::Error::new(io::ErrorKind::InvalidInput, "child pid does not fit pid_t"),
            )
        })?;
        let group = pid.checked_neg().ok_or_else(|| {
            contextual_io(
                "terminate Bash process group",
                io::Error::new(io::ErrorKind::InvalidInput, "child pid cannot be negated"),
            )
        })?;
        // SAFETY: the process was spawned by this adapter with its own group.
        let result = unsafe { libc::kill(group, libc::SIGKILL) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(contextual_io("terminate Bash process group", error));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = process_group;
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
    cancel: CancellationToken,
) -> Result<CapturedRun, ToolError> {
    let control = Arc::new(AtomicU8::new(CONTROL_RUNNING));
    let worker_control = Arc::clone(&control);
    let worker = tokio::task::spawn_blocking(move || {
        run_pty_blocking(
            command,
            env,
            cwd,
            artifact_root,
            timeout.duration,
            worker_control,
        )
    });
    let mut worker = Box::pin(worker);
    let result = tokio::select! {
        result = &mut worker => result,
        _ = cancel.cancelled() => {
            control.store(CONTROL_CANCELLED, Ordering::Release);
            (&mut worker).await
        }
    };
    result.map_err(|error| ToolError::Other(format!("Bash PTY worker failed: {error}")))?
}

/// Spawn and capture a PTY child while polling its nonblocking master.
#[cfg(unix)]
fn run_pty_blocking(
    command: String,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    artifact_root: PathBuf,
    timeout: Option<Duration>,
    control: Arc<AtomicU8>,
) -> Result<CapturedRun, ToolError> {
    use pty_process::blocking::{Command, open};
    use std::os::fd::AsRawFd;

    let mut sink = OutputSink::new(artifact_root);
    if control.load(Ordering::Acquire) == CONTROL_CANCELLED {
        let output = sink
            .finish()
            .map_err(|error| contextual_io("finish Bash PTY output", error))?;
        return Ok(CapturedRun {
            completion: Completion::Cancelled,
            output,
        });
    }
    let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
    let (mut master, pts) =
        open().map_err(|error| ToolError::Other(format!("Bash PTY setup failed: {error}")))?;
    set_nonblocking(master.as_raw_fd())
        .map_err(|error| contextual_io("set Bash PTY master nonblocking", error))?;
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .envs(env)
        .spawn(pts)
        .map_err(|error| ToolError::Other(format!("spawn Bash PTY process: {error}")))?;
    let process_group = child.id();

    let mut buffer = [0_u8; PIPE_BUFFER_BYTES];
    let mut leader_reaped = false;
    let mut group_terminated = false;
    let mut eof = false;
    let mut completion = None;
    loop {
        let requested = control.load(Ordering::Acquire);
        if requested == CONTROL_CANCELLED {
            if !group_terminated {
                terminate_pty_child(&mut child, Some(process_group), leader_reaped)?;
                group_terminated = true;
                leader_reaped = true;
            }
            completion = Some(Completion::Cancelled);
        } else if requested == CONTROL_TIMED_OUT || deadline.is_some_and(|at| Instant::now() >= at)
        {
            control.store(CONTROL_TIMED_OUT, Ordering::Release);
            if !group_terminated {
                terminate_pty_child(&mut child, Some(process_group), leader_reaped)?;
                group_terminated = true;
                leader_reaped = true;
            }
            completion = Some(Completion::TimedOut);
        }

        if !leader_reaped {
            match child.try_wait() {
                Ok(Some(status)) => {
                    leader_reaped = true;
                    completion.get_or_insert(Completion::Finished(status.code()));
                }
                Ok(None) => {}
                Err(error) => {
                    let wait_error = contextual_io("poll Bash PTY process", error);
                    let cleanup = terminate_pty_child(&mut child, Some(process_group), false);
                    return Err(with_cleanup_error(wait_error, cleanup));
                }
            }
        }

        // A PTY master may report EIO/HUP when the leader exits even though a
        // descendant still owns the slave.  EOF is terminal only after the
        // saved process group is absent.
        if leader_reaped && eof && !pty_process_group_alive(process_group) {
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
            let cleanup = terminate_pty_child(&mut child, Some(process_group), leader_reaped);
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
                    let cleanup =
                        terminate_pty_child(&mut child, Some(process_group), leader_reaped);
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
                let cleanup = terminate_pty_child(&mut child, Some(process_group), leader_reaped);
                return Err(with_cleanup_error(read_error, cleanup));
            }
        }
    }
    if !leader_reaped {
        let status = child
            .wait()
            .map_err(|error| contextual_io("reap Bash PTY process", error))?;
        completion.get_or_insert(Completion::Finished(status.code()));
    }
    // Close the master exactly once after all bytes have been observed.
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
    process_group: Option<u32>,
    leader_reaped: bool,
) -> Result<(), ToolError> {
    terminate_pty_process_group(process_group)?;
    if !leader_reaped {
        child
            .wait()
            .map(|_| ())
            .map_err(|error| contextual_io("reap Bash PTY process", error))?;
    }
    Ok(())
}

/// Terminate the process group that owns a PTY slave.
#[cfg(unix)]
fn terminate_pty_process_group(process_group: Option<u32>) -> Result<(), ToolError> {
    let Some(pid) = process_group else {
        return Ok(());
    };
    let pid = libc::pid_t::try_from(pid).map_err(|_| {
        contextual_io(
            "terminate Bash PTY process group",
            io::Error::new(io::ErrorKind::InvalidInput, "child pid does not fit pid_t"),
        )
    })?;
    let group = pid.checked_neg().ok_or_else(|| {
        contextual_io(
            "terminate Bash PTY process group",
            io::Error::new(io::ErrorKind::InvalidInput, "child pid cannot be negated"),
        )
    })?;
    // SAFETY: pty-process makes this child a session/process-group leader.
    let result = unsafe { libc::kill(group, libc::SIGKILL) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(contextual_io("terminate Bash PTY process group", error));
        }
    }
    Ok(())
}
/// Return whether any member remains in the PTY process group.
#[cfg(unix)]
fn pty_process_group_alive(process_group: u32) -> bool {
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
        Completion::TimedOut | Completion::Cancelled => None,
    };
    let mut output_path = captured.output.output_path.clone();
    let mut displayed_output_path = output_path
        .as_deref()
        .map(validate_output_path)
        .transpose()?;
    let mut truncated = output_path.is_some();
    let mut prefix = shell_notice_prefix(timeout, displayed_output_path.as_deref(), timed_out);

    // Measure the untruncated lossy preview first.  Rendering applies the
    // inline cap, so measuring its result would hide a late spill condition.
    if shell_output_len(&prefix, &captured.output.inline) > MAX_OUTPUT_BYTES
        && output_path.is_none()
    {
        captured
            .output
            .spill_inline()
            .map_err(|error| contextual_io("spill Bash output capture", error))?;
        output_path = captured.output.output_path.clone();
        displayed_output_path = output_path
            .as_deref()
            .map(validate_output_path)
            .transpose()?;
        truncated = output_path.is_some();
        prefix = shell_notice_prefix(timeout, displayed_output_path.as_deref(), timed_out);
    }
    let output = render_shell_output(&prefix, &captured.output.inline);

    let mut metadata = Map::new();
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
) -> String {
    let mut prefix = String::new();
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

/// Render a notice followed by a UTF-8-safe preview within the serialized cap.
fn render_shell_output(prefix: &str, inline: &[u8]) -> String {
    let mut output = prefix.to_owned();
    if inline.is_empty() {
        output.push_str("(no output)");
        return output;
    }
    let mut text = String::from_utf8_lossy(inline).into_owned();
    let available = MAX_OUTPUT_BYTES.saturating_sub(serialized_string_len(&output));
    truncate_utf8_for_json(&mut text, available);
    output.push_str(&text);
    output
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

/// Require external-directory permission for cwd values outside the workdir.
async fn assert_external_workdir(ctx: &ToolCtx, cwd: &Path) -> Result<(), ToolError> {
    let base = normalize(&absolutize(&ctx.workdir));
    let cwd = normalize(&absolutize(cwd));
    if cwd.starts_with(&base) {
        return Ok(());
    }
    let pattern = display_path(&cwd.join("*"));
    ctx.permission
        .assert(Action::ExternalDirectory, Resource::Path(pattern))
        .await?;
    Ok(())
}
