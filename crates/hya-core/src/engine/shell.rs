use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId, ToolCallId, ToolName};
use hya_tool::{ToolCtx, ToolError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{
    AgentSpec, SessionEngine, agent_roster, authorize_tool_call, effective_agent_for_binding,
    session_workdir,
};
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use crate::runtime_registry::CompiledResourceView;

mod admission;
mod hooks;

use hooks::{AfterHookCall, apply_tool_after_hooks};

struct ShellPart {
    session: SessionId,
    message: MessageId,
    part: PartId,
    call: ToolCallId,
    name: ToolName,
}

/// One validated private Bash artifact held until its durable result publishes.
struct OwnedBashArtifact {
    path: PathBuf,
    output_path: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Delete a pre-hook Bash spill unless the durable result retains its exact path.
pub(super) struct BashArtifactGuard {
    bash: bool,
    artifact: Option<OwnedBashArtifact>,
}

impl BashArtifactGuard {
    /// Capture only a private artifact emitted by the executed Bash tool beneath
    /// the bound workdir's canonical tool-output directory.
    pub(super) fn capture(tool: &str, output: Option<&Value>, workdir: &Path) -> Self {
        let bash = tool == "bash" || tool == "shell";
        let artifact = if bash {
            output.and_then(|output| validated_bash_artifact(output, workdir))
        } else {
            None
        };
        Self { bash, artifact }
    }

    /// Return whether the post-hook, post-cap result exposes the exact captured path.
    pub(super) fn retained_by(&self, output: &Value) -> bool {
        let Some(artifact) = self.artifact.as_ref() else {
            return false;
        };
        output
            .get("metadata")
            .and_then(|metadata| metadata.get("outputPath"))
            .and_then(Value::as_str)
            == Some(artifact.output_path.as_str())
    }

    /// Remove an output path introduced or changed by a Bash after-hook.
    pub(super) fn remove_unowned_path(&self, output: &mut Value) {
        if !self.bash || self.retained_by(output) {
            return;
        }
        if let Some(Value::Object(metadata)) = output.get_mut("metadata") {
            metadata.remove("outputPath");
        }
    }

    /// Delete the captured artifact after a hook removes or rejects its path.
    ///
    /// # Errors
    /// Returns a contextual core/tool I/O error when identity revalidation or
    /// removal fails. The guard remains armed so unwinding retries cleanup.
    pub(super) fn discard(&mut self) -> Result<(), CoreError> {
        let Some(artifact) = self.artifact.as_ref() else {
            return Ok(());
        };
        match remove_owned_bash_artifact(artifact) {
            Ok(()) => {
                self.artifact = None;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.artifact = None;
                Ok(())
            }
            Err(error) => Err(ToolError::Io(error).into()),
        }
    }

    /// Disarm cleanup after the durable ToolResult containing the path publishes.
    pub(super) fn disarm(&mut self) {
        self.artifact = None;
    }
}

impl Drop for BashArtifactGuard {
    fn drop(&mut self) {
        if let Some(artifact) = self.artifact.as_ref() {
            let _ = remove_owned_bash_artifact(artifact);
        }
    }
}

/// Validate a pre-hook Bash output path without trusting hook-provided metadata.
fn validated_bash_artifact(output: &Value, workdir: &Path) -> Option<OwnedBashArtifact> {
    let output_path = output
        .get("metadata")?
        .get("outputPath")?
        .as_str()?
        .to_owned();
    let supplied = Path::new(&output_path);
    if !supplied.is_absolute() {
        return None;
    }
    let file_name = supplied.file_name()?.to_str()?;
    if !file_name.starts_with("tool_") || !file_name.ends_with(".txt") {
        return None;
    }
    let root = std::fs::canonicalize(workdir.join(".hya/tool-output")).ok()?;
    let parent = std::fs::canonicalize(supplied.parent()?).ok()?;
    if parent != root {
        return None;
    }
    let metadata = std::fs::symlink_metadata(supplied).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return None;
    }
    Some(OwnedBashArtifact {
        path: supplied.to_path_buf(),
        output_path,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

/// Revalidate the captured inode immediately before deleting the artifact path.
fn remove_owned_bash_artifact(artifact: &OwnedBashArtifact) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(&artifact.path)?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other(
            "Bash output artifact is no longer a regular file",
        ));
    }
    #[cfg(unix)]
    if metadata.dev() != artifact.device || metadata.ino() != artifact.inode {
        return Err(io::Error::other(
            "Bash output artifact identity changed before cleanup",
        ));
    }
    std::fs::remove_file(&artifact.path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("remove unpublished Bash output artifact: {error}"),
        )
    })
}

impl SessionEngine {
    /// Run a shell command as a session operation with hooks and permissions.
    pub async fn run_shell(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        command: String,
        cancel: CancellationToken,
    ) -> Result<(MessageId, FinishReason), CoreError> {
        self.admit_shell_user_message(session).await?;
        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let binding = self.bind_root_runtime(&workdir).await?;
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&agent.name)
            .as_str();
        // Shell turns do not attach project/reference guidance.
        let (agent, resources) = effective_agent_for_binding(agent, stable_id, &binding, None)?;

        let message = MessageId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TurnBindingRecorded {
                session,
                message,
                generation: binding.generation(),
            },
        )
        .await?;

        let part = PartId::new();
        let call = ToolCallId::new();
        let name = ToolName::new("bash");
        self.emit(
            session,
            Event::ToolInputStart {
                session,
                message,
                part,
                call,
                name: name.clone(),
            },
        )
        .await?;

        let finish = self
            .execute_shell_part(
                ShellPart {
                    session,
                    message,
                    part,
                    call,
                    name,
                },
                command,
                &binding,
                &agent,
                &resources,
                cancel,
            )
            .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish,
                tokens: None,
            },
        )
        .await?;
        Ok((message, finish))
    }

    async fn execute_shell_part(
        &self,
        shell_part: ShellPart,
        command: String,
        binding: &TurnBinding,
        agent: &AgentSpec,
        resources: &CompiledResourceView,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        let session = shell_part.session;
        let tool = shell_part.name.to_string();
        let mut input = json!({ "command": command });
        if let Some(hooks) = &self.hooks {
            let current = std::mem::take(&mut input);
            match hooks
                .tool_execute_before(ToolExecuteBeforeInput {
                    session,
                    message: shell_part.message,
                    call: shell_part.call,
                    tool: tool.clone(),
                    input: current,
                })
                .await
            {
                ToolExecuteBeforeOutcome::Continue { input: next } => input = next,
                ToolExecuteBeforeOutcome::Veto { reason } => {
                    let message_text = format!("blocked by plugin: {reason}");
                    self.emit(
                        session,
                        Event::ToolError {
                            session,
                            message: shell_part.message,
                            part: shell_part.part,
                            call: shell_part.call,
                            value: Some(tool_error_message_value("blocked", &message_text)),
                            message_text,
                        },
                    )
                    .await?;
                    return Ok(FinishReason::Error);
                }
            }
        }

        self.emit(
            session,
            Event::ToolCallRequested {
                session,
                message: shell_part.message,
                part: shell_part.part,
                call: shell_part.call,
                name: shell_part.name,
                input: input.clone(),
            },
        )
        .await?;

        let projection = self.store.read_projection(session).await?;
        let input_for_after = self.hooks.as_ref().map(|_| input.clone());
        let started = std::time::Instant::now();
        let result = match resources.resolve_tool(&tool) {
            Some(resolved) => match authorize_tool_call(
                &resolved,
                &input,
                self.permission.for_session(session),
                shell_part.message,
                shell_part.call,
            )
            .await
            {
                Ok(permission) => {
                    let ctx = ToolCtx {
                        workflows: hya_tool::WorkflowPlane::disconnected(),
                        permission,
                        interaction: self.interaction.for_session(session),
                        spawner: self.spawner.for_binding(binding).for_session_with_agents(
                            session,
                            agent_roster(binding, agent.name.as_str())?,
                        ),
                        operation: hya_tool::ToolOperation::from_tool_call(shell_part.call),
                        mailbox: self.mailbox.for_session(session),
                        session: Some(session),
                        parent_session: projection.session.parent,
                        todo: self.todo.clone(),
                        skills: resources.skill_plane(),
                        agents: agent_roster(binding, agent.name.as_str())?,
                        websearch: self.websearch.clone(),
                        lsp: self.lsp.clone(),
                        formatter: self.formatter.clone(),
                        workdir: binding.workdir().to_path_buf(),
                        cancel,
                    };
                    resolved.tool.execute(&ctx, input).await
                }
                Err(error) => Err(error),
            },
            None => Err(ToolError::Other("unknown tool: bash".to_string())),
        };
        let mut artifact_guard =
            BashArtifactGuard::capture(&tool, result.as_ref().ok(), binding.workdir());
        let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = apply_tool_after_hooks(
            self,
            result,
            AfterHookCall {
                session,
                message: shell_part.message,
                call: shell_part.call,
                tool: &tool,
                input: input_for_after,
                time_ms,
            },
        )
        .await;

        match result {
            Ok(mut output) => {
                artifact_guard.remove_unowned_path(&mut output);
                if !artifact_guard.retained_by(&output) {
                    artifact_guard.discard()?;
                }
                // Direct shell calls bypass the normal turn loop, so apply the
                // same shape-aware coding cap after hooks and before durable
                // event publication.  A hook must not re-expand the envelope.
                let output = hya_tool::cap_tool_output_with_policy(
                    output,
                    hya_tool::ToolResultPolicy::Coding,
                );
                let retains_artifact = artifact_guard.retained_by(&output);
                self.emit(
                    session,
                    Event::ToolResult {
                        session,
                        message: shell_part.message,
                        part: shell_part.part,
                        call: shell_part.call,
                        output,
                        time_ms,
                    },
                )
                .await?;
                if retains_artifact {
                    artifact_guard.disarm();
                } else {
                    artifact_guard.discard()?;
                }
                Ok(FinishReason::Stop)
            }
            Err(error) => {
                artifact_guard.discard()?;
                let finish = finish_from_tool_error(&error);
                self.emit(
                    session,
                    Event::ToolError {
                        session,
                        message: shell_part.message,
                        part: shell_part.part,
                        call: shell_part.call,
                        value: Some(tool_error_value(&error)),
                        message_text: error.to_string(),
                    },
                )
                .await?;
                Ok(finish)
            }
        }
    }
}

fn finish_from_tool_error(error: &ToolError) -> FinishReason {
    if matches!(error, ToolError::Cancelled) {
        FinishReason::Cancelled
    } else {
        FinishReason::Error
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// Reject a lexical artifact entry that was swapped to a sibling symlink.
    #[test]
    fn validated_bash_artifact_rejects_symlink_entry() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workdir = std::env::temp_dir().join(format!(
            "hya-core-bash-artifact-{nonce}-{}",
            std::process::id()
        ));
        let root = workdir.join(".hya/tool-output");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"private").unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
        let supplied = root.join("tool_swapped.txt");
        symlink(&victim, &supplied).unwrap();
        let output = serde_json::json!({
            "metadata": {"outputPath": supplied.to_string_lossy()}
        });

        let artifact = validated_bash_artifact(&output, &workdir);

        assert!(artifact.is_none());
        assert_eq!(std::fs::read(&victim).unwrap(), b"private");
        std::fs::remove_dir_all(workdir).unwrap();
    }
}
