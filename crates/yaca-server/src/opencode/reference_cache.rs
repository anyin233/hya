use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

pub(super) fn materialize(repository: &str, branch: Option<&str>, path: PathBuf) {
    let Some(reference) = super::reference_repository::parse(repository) else {
        return;
    };
    if path.join(".git").is_dir() || !mark_active(&path) {
        return;
    }
    let remote = super::reference_repository::remote(&reference).to_string();
    let branch = branch.map(ToString::to_string);
    tokio::spawn(async move {
        if let Err(error) = ensure(&remote, branch.as_deref(), &path).await {
            tracing::warn!(
                %error,
                repository = %remote,
                path = %path.display(),
                "failed to materialize opencode reference"
            );
        }
        unmark_active(&path);
    });
}

fn active_paths() -> &'static Mutex<BTreeSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn mark_active(path: &Path) -> bool {
    let Ok(mut active) = active_paths().lock() else {
        return false;
    };
    active.insert(path.to_path_buf())
}

fn unmark_active(path: &Path) {
    if let Ok(mut active) = active_paths().lock() {
        active.remove(path);
    }
}

async fn ensure(remote: &str, branch: Option<&str>, path: &Path) -> Result<(), String> {
    if path.join(".git").is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    if path.exists() {
        remove_existing(path)?;
    }
    let mut command = Command::new("git");
    command.arg("clone").arg("--depth").arg("1");
    if let Some(branch) = branch {
        command.arg("--branch").arg(branch);
    }
    command.arg(remote).arg(path).kill_on_drop(true);
    let output = timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "git clone timed out".to_string())?
        .map_err(|error| error.to_string())?;
    output.status.success().then_some(()).ok_or_else(|| {
        let stderr = output_text(&output.stderr);
        if stderr.is_empty() {
            "git clone failed".to_string()
        } else {
            stderr
        }
    })
}

fn remove_existing(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        std::fs::remove_file(path).map_err(|error| error.to_string())
    }
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}
