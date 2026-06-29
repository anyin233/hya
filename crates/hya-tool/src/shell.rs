use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::lsp_path::{absolutize, display_path, normalize, resolve_file};
use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
static NEXT_OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

enum ShellWait {
    Completed(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Cancelled,
}

#[derive(Deserialize)]
struct ShellInput {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

pub(crate) struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("shell"),
            description: "Run a shell command in the working dir.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout": { "type": "integer", "minimum": 1 },
                    "workdir": { "type": "string" },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["command"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let input: ShellInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let ShellInput {
            command,
            timeout,
            workdir,
            env,
        } = input;
        let timeout_ms = timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms == 0 {
            return Err(ToolError::Input(
                "timeout must be greater than 0".to_string(),
            ));
        }
        ctx.permission
            .assert(Action::Bash, Resource::Command(command.clone()))
            .await?;

        let cwd = cwd(ctx, workdir.as_deref());
        assert_external_workdir(ctx, &cwd).await?;
        let mut child = {
            let mut proc = tokio::process::Command::new("sh");
            proc.arg("-c")
                .arg(&command)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            #[cfg(unix)]
            proc.process_group(0);
            if !env.is_empty() {
                proc.envs(&env);
            }
            proc.spawn()?
        };

        let read_stdout = tokio::spawn(read_pipe(child.stdout.take()));
        let read_stderr = tokio::spawn(read_pipe(child.stderr.take()));
        let wait = tokio::select! {
            () = ctx.cancel.cancelled() => ShellWait::Cancelled,
            result = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()) => {
                match result {
                    Ok(status) => ShellWait::Completed(status),
                    Err(_) => ShellWait::TimedOut,
                }
            }
        };

        let (status_code, timed_out) = match wait {
            ShellWait::Completed(status) => (status?.code(), false),
            ShellWait::TimedOut => {
                terminate_child(&mut child).await;
                (None, true)
            }
            ShellWait::Cancelled => {
                terminate_child(&mut child).await;
                let _ = read_stdout.await;
                let _ = read_stderr.await;
                return Err(ToolError::Cancelled);
            }
        };
        let stdout = read_stdout
            .await
            .map_err(|e| ToolError::Other(e.to_string()))??;
        let stderr = read_stderr
            .await
            .map_err(|e| ToolError::Other(e.to_string()))??;
        let raw_output = combined_output(&stdout, &stderr);
        let mut output = raw_output.clone();
        if output.is_empty() {
            output = "(no output)".to_string();
        }
        let truncated = output.len() > MAX_OUTPUT_BYTES;
        let output_path = if truncated {
            Some(save_truncated_output(ctx, &raw_output).await?)
        } else {
            None
        };
        if truncated {
            let file = output_path
                .as_ref()
                .map(|path| display_path(path))
                .unwrap_or_default();
            output = format!(
                "...output truncated...\n\nFull output saved to: {file}\n\n{}",
                truncate(&raw_output)
            );
        }
        if timed_out {
            output.push_str(&format!(
                "\n\n<shell_metadata>\nshell tool terminated command after exceeding timeout {timeout_ms} ms. If this command is expected to take longer and is not waiting for interactive input, retry with a larger timeout value in milliseconds.\n</shell_metadata>"
            ));
        }
        let mut metadata = Map::from_iter([
            ("output".to_string(), json!(truncate(&raw_output))),
            ("exit".to_string(), json!(status_code)),
            ("truncated".to_string(), json!(truncated)),
        ]);
        if let Some(output_path) = output_path {
            metadata.insert("outputPath".to_string(), json!(display_path(&output_path)));
        }

        Ok(json!({
            "stdout": truncate(&stdout),
            "stderr": truncate(&stderr),
            "exit_code": status_code,
            "title": command,
            "output": output,
            "metadata": metadata,
        }))
    }
}

#[cfg(unix)]
async fn terminate_child(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: pid is the spawned child id; a negative pid targets its process group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(not(unix))]
async fn terminate_child(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn cwd(ctx: &ToolCtx, workdir: Option<&str>) -> PathBuf {
    let base = normalize(&absolutize(&ctx.workdir));
    workdir.map_or(base.clone(), |path| resolve_file(&base, path))
}

async fn read_pipe<R>(pipe: Option<R>) -> Result<String, ToolError>
where
    R: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut buf = Vec::new();
    pipe.read_to_end(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}{stderr}"),
    }
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[truncated {} bytes]", &s[..end], s.len() - end)
}

async fn save_truncated_output(ctx: &ToolCtx, output: &str) -> Result<PathBuf, ToolError> {
    let dir = normalize(&absolutize(&ctx.workdir)).join(".hya/tool-output");
    tokio::fs::create_dir_all(&dir).await?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let id = NEXT_OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("tool_{millis}_{}_{id}.txt", std::process::id()));
    tokio::fs::write(&path, output).await?;
    Ok(path)
}

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
