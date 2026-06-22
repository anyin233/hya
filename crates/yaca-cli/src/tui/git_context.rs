use std::path::Path;
use std::process::Command;
use yaca_tui::AppState;

pub(super) fn branch_label_from_workdir(workdir: Option<&str>) -> Option<String> {
    workdir.and_then(|path| branch_label(Path::new(path)))
}

pub(super) fn refresh_app_branch(app: &mut AppState) {
    app.branch_label = branch_label_from_workdir(app.projection.session.workdir.as_deref());
}

pub(super) fn branch_label(workdir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod tests {
    use super::{branch_label, refresh_app_branch};

    use anyhow::ensure;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};
    use yaca_tui::AppState;

    #[test]
    fn branch_label_reads_current_branch_when_workdir_is_git_repo() -> anyhow::Result<()> {
        let Some(git) = git_command() else {
            return Ok(());
        };
        let repo = unique_temp_dir("yaca-branch-label-repo");
        fs::create_dir_all(&repo)?;

        // Given: a real git worktree checked out on a named branch.
        run_git(&git, &repo, &["init"])?;
        run_git(&git, &repo, &["checkout", "-b", "feat/opencode-footer"])?;

        // When: the TUI asks for the worktree branch label.
        let label = branch_label(&repo);

        // Then: the footer label uses the actual branch name.
        assert_eq!(label.as_deref(), Some("feat/opencode-footer"));

        let _ = fs::remove_dir_all(repo);
        Ok(())
    }

    #[test]
    fn branch_label_is_none_when_workdir_is_not_git_repo() -> anyhow::Result<()> {
        let workdir = unique_temp_dir("yaca-branch-label-plain");
        fs::create_dir_all(&workdir)?;

        // Given: a normal directory with no git metadata.
        // When: the TUI asks for a branch label.
        let label = branch_label(&workdir);

        // Then: the sidebar omits the branch row rather than inventing one.
        assert_eq!(label, None);

        let _ = fs::remove_dir_all(workdir);
        Ok(())
    }

    #[test]
    fn refresh_app_branch_tracks_git_branch_changes() -> anyhow::Result<()> {
        let Some(git) = git_command() else {
            return Ok(());
        };
        let repo = unique_temp_dir("yaca-branch-refresh-repo");
        fs::create_dir_all(&repo)?;
        run_git(&git, &repo, &["init"])?;
        run_git(&git, &repo, &["checkout", "-b", "feat/before"])?;
        let mut app = AppState::default();
        app.projection.session.workdir = Some(repo.to_string_lossy().into_owned());

        // Given: the TUI state points at a git worktree.
        refresh_app_branch(&mut app);
        assert_eq!(app.branch_label.as_deref(), Some("feat/before"));

        // When: the worktree branch changes while the TUI remains open.
        run_git(&git, &repo, &["checkout", "-b", "feat/after"])?;
        refresh_app_branch(&mut app);

        // Then: the app footer state follows the current branch.
        assert_eq!(app.branch_label.as_deref(), Some("feat/after"));

        let _ = fs::remove_dir_all(repo);
        Ok(())
    }

    fn git_command() -> Option<PathBuf> {
        let output = Command::new("git").arg("--version").output().ok()?;
        output.status.success().then(|| PathBuf::from("git"))
    }

    fn run_git(git: &Path, cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
        let output = Command::new(git).arg("-C").arg(cwd).args(args).output()?;
        ensure!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }
}
