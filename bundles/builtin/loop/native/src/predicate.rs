//! Deterministic shell-command exit predicates (`--while` / `--until`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_runtime_abi::HostError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default evaluation timeout for shell predicates (30 seconds).
pub const DEFAULT_PREDICATE_TIMEOUT_SECS: u64 = 30;

/// Which direction a [`LoopPredicate`] reads in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateMode {
    /// Exit 0 continues the loop; exit 1 stops it.
    While,
    /// Exit 0 stops the loop; exit 1 continues it.
    Until,
}

impl PredicateMode {
    /// Wire/config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::While => "while",
            Self::Until => "until",
        }
    }
}

/// The result of one deterministic predicate evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoopPredicateOutcome {
    /// The stop condition is met (exit code matched the mode's stop answer).
    Satisfied {
        /// Human-readable stop reason for the run outcome.
        reason: String,
    },
    /// Keep iterating.
    Continue,
    /// The condition itself is broken (non-answer exit code or timeout).
    /// Stopping here is a failure, never a success: a typo'd condition must
    /// not look like finished work.
    Broken {
        /// What made the condition unusable.
        detail: String,
    },
}

/// Wire JSON representation of a deterministic loop predicate passed in
/// [`crate::LoopRunRequest::predicate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopPredicateSpec {
    /// Shell command text, executed as `sh -c <command>`.
    pub command: String,
    /// Answer direction (`while` or `until`).
    pub mode: PredicateMode,
    /// Optional working directory override (falls back to session workdir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    /// Optional evaluation timeout in seconds (defaults to 30s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Deterministic loop-exit predicate: `--while '<cmd>'` / `--until '<cmd>'`.
///
/// The exit code is the only signal — stdout is ignored. Exit 0/1 are the
/// condition's answer (inverted between [`PredicateMode::While`] and
/// [`PredicateMode::Until`]); any other exit code or a timeout means the
/// condition itself is broken, which stops the loop as a failure rather than
/// silently looking like finished work.
#[derive(Clone, Debug)]
pub struct LoopPredicate {
    /// Shell command text, run as `sh -c <command>` in `workdir`.
    pub command: String,
    /// Answer direction.
    pub mode: PredicateMode,
    /// Evaluation timeout; a timed-out condition is broken, not false.
    pub timeout: Duration,
    /// Working directory for the command.
    pub workdir: PathBuf,
}

impl LoopPredicate {
    /// Build a predicate with the default 30s evaluation timeout: a condition
    /// that hangs is a broken condition, never a false answer.
    #[must_use]
    pub fn new(command: impl Into<String>, mode: PredicateMode, workdir: PathBuf) -> Self {
        Self {
            command: command.into(),
            mode,
            timeout: Duration::from_secs(DEFAULT_PREDICATE_TIMEOUT_SECS),
            workdir,
        }
    }

    /// Parse a [`LoopPredicate`] from the JSON payload carried on
    /// [`crate::LoopRunRequest::predicate`].
    ///
    /// # Errors
    /// Returns [`HostError::InvalidInput`] if the JSON payload is malformed or
    /// carries an empty command.
    pub fn from_json_value(value: &Value, fallback_workdir: &Path) -> Result<Self, HostError> {
        let spec: LoopPredicateSpec = serde_json::from_value(value.clone())
            .map_err(|error| HostError::InvalidInput(format!("invalid loop predicate: {error}")))?;
        if spec.command.trim().is_empty() {
            return Err(HostError::InvalidInput(
                "loop predicate command must not be empty".to_string(),
            ));
        }
        let timeout_secs = spec.timeout_secs.unwrap_or(DEFAULT_PREDICATE_TIMEOUT_SECS);
        if timeout_secs == 0 {
            return Err(HostError::InvalidInput(
                "loop predicate timeout_secs must be greater than zero".to_string(),
            ));
        }
        Ok(Self {
            command: spec.command,
            mode: spec.mode,
            timeout: Duration::from_secs(timeout_secs),
            workdir: spec
                .workdir
                .unwrap_or_else(|| fallback_workdir.to_path_buf()),
        })
    }

    /// Evaluate asynchronously. Dropping this future terminates the child,
    /// allowing the iteration driver's cancellation to interrupt the wait.
    pub async fn evaluate(&self) -> LoopPredicateOutcome {
        let Ok(mut child) = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .current_dir(&self.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        else {
            return LoopPredicateOutcome::Broken {
                detail: format!("failed to spawn condition command `{}`", self.command),
            };
        };
        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                return LoopPredicateOutcome::Broken {
                    detail: format!("condition wait failed: {error}"),
                };
            }
            Err(_) => {
                let _ = child.kill().await;
                return LoopPredicateOutcome::Broken {
                    detail: format!(
                        "condition `{}` timed out after {:?}",
                        self.command, self.timeout
                    ),
                };
            }
        };
        let Some(code) = status.code() else {
            return LoopPredicateOutcome::Broken {
                detail: format!(
                    "condition `{}` terminated by signal; the condition itself is broken",
                    self.command
                ),
            };
        };
        match (self.mode, code) {
            (PredicateMode::Until, 0) | (PredicateMode::While, 1) => {
                LoopPredicateOutcome::Satisfied {
                    reason: format!(
                        "{} `{}` answered with exit {code}",
                        self.mode.as_str(),
                        self.command
                    ),
                }
            }
            (PredicateMode::Until, 1) | (PredicateMode::While, 0) => LoopPredicateOutcome::Continue,
            _ => LoopPredicateOutcome::Broken {
                detail: format!(
                    "condition `{}` exited {code}; the condition itself is broken",
                    self.command
                ),
            },
        }
    }
}
