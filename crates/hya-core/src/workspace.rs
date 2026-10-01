use std::path::Path;
use tokio::process::Command;

use crate::error::CoreError;

/// Opens a tmux pane per member for human observability. tmux is NOT the source
/// of truth — it tails the member session. Degrades with a clear error if tmux
/// is unavailable.
pub struct TmuxPaneManager {
    session: String,
}

impl TmuxPaneManager {
    #[must_use]
    /// Construct a manager for the given root/session context.
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
        }
    }

    /// Whether tmux is available on this host.
    pub async fn available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Open a tmux pane running `tail_command` in `cwd`.
    pub async fn open(&self, cwd: &Path, tail_command: &str) -> Result<String, CoreError> {
        if !Self::available().await {
            return Err(CoreError::Invalid("tmux is not available".to_string()));
        }
        let cwd_str = cwd.to_string_lossy().into_owned();
        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &self.session,
                "-c",
                &cwd_str,
                "-P",
                "-F",
                "#{pane_id}",
                tail_command,
            ])
            .output()
            .await
            .map_err(|e| CoreError::Invalid(format!("tmux spawn failed: {e}")))?;
        if !output.status.success() {
            return Err(CoreError::Invalid(format!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
