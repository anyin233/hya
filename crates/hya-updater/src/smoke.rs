//! Subprocess smoke of a staged generation.
//!
//! Smoke never loads candidate code into the updater process address space.
//! It only executes a path under the staged release directory in a dedicated
//! child process. This does not claim OS sandbox isolation.

use std::path::Path;
use std::process::Command;

use crate::error::UpdaterError;
use crate::stage::StagedRelease;

/// Run a relative smoke command under the staged release directory.
///
/// `relative_command` must resolve inside the staged directory (no `..` escape).
pub fn smoke_staged_release(
    staged: &StagedRelease,
    relative_command: &str,
    args: &[&str],
) -> Result<(), UpdaterError> {
    if relative_command.is_empty() {
        return Err(UpdaterError::SmokeFailed(
            "smoke command must be non-empty".to_string(),
        ));
    }
    if Path::new(relative_command).is_absolute()
        || relative_command.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(UpdaterError::SmokeFailed(
            "smoke command must be a relative path without `..`".to_string(),
        ));
    }
    let dir = staged.directory();
    let command_path = dir.join(relative_command);
    if !command_path.is_file() {
        return Err(UpdaterError::SmokeFailed(format!(
            "smoke binary missing: {}",
            command_path.display()
        )));
    }
    let status = Command::new(&command_path)
        .args(args)
        .current_dir(&dir)
        .status()
        .map_err(|error| {
            UpdaterError::SmokeFailed(format!("spawn smoke `{}`: {error}", command_path.display()))
        })?;
    if !status.success() {
        return Err(UpdaterError::SmokeFailed(format!(
            "smoke exited with {status}"
        )));
    }
    Ok(())
}
