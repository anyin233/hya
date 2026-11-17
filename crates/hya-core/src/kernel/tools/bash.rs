use std::io::Read as _;
use std::process::{ExitStatus, Stdio};

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

pub(super) struct Bash;

#[derive(Deserialize)]
struct Args {
    command: String,
}

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "bash",
            "Run a shell command (`sh -c`) in the working directory. Returns its exit code and combined stdout and stderr.",
            json!({
                "type": "object",
                "properties": {"command": {"type": "string", "description": "Shell command"}},
                "required": ["command"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { command } = super::args(input)?;
        // One pipe for both streams keeps their interleaving.
        let (mut reader, writer) = std::io::pipe()?;
        let mut process = Command::new("sh");
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
        let output = tokio::task::spawn_blocking(move || {
            let mut output = Vec::new();
            reader.read_to_end(&mut output).map(|_| output)
        });
        let status = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            status = child.wait() => status?,
        };
        // Background jobs the command left behind would hold the pipe open.
        drop(group);
        let output = output
            .await
            .map_err(|error| ToolError::Other(format!("bash output reader: {error}")))??;
        Ok(json!({
            "exit_code": exit_code(status),
            "output": String::from_utf8_lossy(&output),
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

/// The command's process group, killed when dropped (on completion, error,
/// or cancellation) so no process it started outlives the call.
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
