use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn directory_for_id(source: &Path, id: &str) -> Option<PathBuf> {
    let primary = canonical_text(source);
    let text = git(source, &["worktree", "list", "--porcelain"])?;
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("worktree ").map(str::trim))
        .filter(|path| canonical_text(Path::new(path)) != primary)
        .find(|path| super::workspace_id::workspace(path) == id)
        .map(PathBuf::from)
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn canonical_text(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
