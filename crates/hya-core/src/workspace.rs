use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hya_proto::TeamRunId;
use tokio::process::Command;

use crate::error::CoreError;

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, CoreError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| CoreError::Invalid(format!("git spawn failed: {e}")))?;
    if !output.status.success() {
        return Err(CoreError::Invalid(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Manages git worktrees for team members. Owns only the worktrees it created
/// (tracked in a registry); cleanup never touches paths it does not own.
pub struct WorktreeManager {
    repo_root: PathBuf,
    owned: Mutex<HashMap<TeamRunId, Vec<PathBuf>>>,
}

impl WorktreeManager {
    #[must_use]
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            owned: Mutex::new(HashMap::new()),
        }
    }

    pub async fn allocate(
        &self,
        team: TeamRunId,
        role: &str,
        base: &str,
    ) -> Result<PathBuf, CoreError> {
        let dir = self
            .repo_root
            .join(".hya/worktrees")
            .join(format!("{team}-{role}"));
        let branch = format!("hya/{team}/{role}");
        let dir_str = dir.to_string_lossy().into_owned();
        run_git(
            &self.repo_root,
            &["worktree", "add", "-b", &branch, &dir_str, base],
        )
        .await?;
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(team)
            .or_default()
            .push(dir.clone());
        Ok(dir)
    }

    pub async fn dirty(&self, dir: &Path) -> Result<bool, CoreError> {
        let out = run_git(dir, &["status", "--porcelain"]).await?;
        Ok(!out.trim().is_empty())
    }

    #[must_use]
    pub fn owned_paths(&self, team: TeamRunId) -> Vec<PathBuf> {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&team)
            .cloned()
            .unwrap_or_default()
    }

    pub async fn release_all(&self, team: TeamRunId) -> Result<(), CoreError> {
        let dirs = self
            .owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&team)
            .unwrap_or_default();
        for dir in dirs {
            let dir_str = dir.to_string_lossy().into_owned();
            run_git(
                &self.repo_root,
                &["worktree", "remove", "--force", &dir_str],
            )
            .await?;
        }
        Ok(())
    }
}

/// Opens a tmux pane per member for human observability. tmux is NOT the source
/// of truth — it tails the member session. Degrades with a clear error if tmux
/// is unavailable.
pub struct TmuxPaneManager {
    session: String,
}

impl TmuxPaneManager {
    #[must_use]
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
        }
    }

    pub async fn available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

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
