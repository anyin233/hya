use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use yaca_tui::{AppState, ChangedFileView};

pub(super) fn refresh_app_branch(app: &mut AppState) {
    let workdir = app.projection.session.workdir.as_deref().map(Path::new);
    app.branch_label = workdir.and_then(branch_label);
    app.changed_files = workdir.map(changed_files).unwrap_or_default();
}

pub(super) fn branch_label(workdir: &Path) -> Option<String> {
    let branch = git_stdout(workdir, ["branch", "--show-current"])?
        .trim()
        .to_string();
    (!branch.is_empty()).then_some(branch)
}

pub(super) fn changed_files(workdir: &Path) -> Vec<ChangedFileView> {
    let Some(status) = git_stdout(workdir, ["status", "--short", "--untracked-files=all"]) else {
        return Vec::new();
    };
    let numstat = changed_file_numstat(workdir);
    status
        .lines()
        .filter_map(status_path)
        .map(|path| {
            let counts = numstat.get(&path).copied();
            ChangedFileView {
                path,
                additions: counts.map(|(additions, _)| additions),
                deletions: counts.map(|(_, deletions)| deletions),
            }
        })
        .collect()
}

fn changed_file_numstat(workdir: &Path) -> BTreeMap<String, (u32, u32)> {
    let mut stats = BTreeMap::new();
    if let Some(output) = git_stdout(workdir, ["diff", "--numstat", "--"]) {
        merge_numstat(&mut stats, &output);
    }
    if let Some(output) = git_stdout(workdir, ["diff", "--cached", "--numstat", "--"]) {
        merge_numstat(&mut stats, &output);
    }
    stats
}

fn merge_numstat(stats: &mut BTreeMap<String, (u32, u32)>, output: &str) {
    for line in output.lines() {
        let mut parts = line.split('\t');
        let Some(additions) = parts.next().and_then(parse_numstat_count) else {
            continue;
        };
        let Some(deletions) = parts.next().and_then(parse_numstat_count) else {
            continue;
        };
        let Some(path) = parts.next().and_then(clean_path) else {
            continue;
        };
        let entry = stats.entry(path).or_default();
        entry.0 = entry.0.saturating_add(additions);
        entry.1 = entry.1.saturating_add(deletions);
    }
}

fn parse_numstat_count(value: &str) -> Option<u32> {
    value.parse::<u32>().ok()
}

fn status_path(line: &str) -> Option<String> {
    let raw = line.get(3..)?;
    let path = raw.split_once(" -> ").map_or(raw, |(_, after)| after);
    clean_path(path)
}

fn clean_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn git_stdout<const N: usize>(workdir: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("-c")
        .arg("core.quotePath=false")
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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

    #[test]
    fn refresh_app_branch_tracks_modified_files() -> anyhow::Result<()> {
        let Some(git) = git_command() else {
            return Ok(());
        };
        let repo = unique_temp_dir("yaca-changed-files-repo");
        fs::create_dir_all(&repo)?;
        run_git(&git, &repo, &["init"])?;
        run_git(&git, &repo, &["config", "user.email", "yaca@example.test"])?;
        run_git(&git, &repo, &["config", "user.name", "yaca"])?;
        fs::write(repo.join("tracked.txt"), "one\n")?;
        run_git(&git, &repo, &["add", "tracked.txt"])?;
        run_git(&git, &repo, &["commit", "-m", "seed"])?;
        fs::write(repo.join("tracked.txt"), "one\ntwo\n")?;
        let mut app = AppState::default();
        app.projection.session.workdir = Some(repo.to_string_lossy().into_owned());

        // Given: the TUI state points at a git worktree with one changed file.
        refresh_app_branch(&mut app);

        // Then: the context rail state carries the modified path and numstat.
        assert_eq!(app.changed_files.len(), 1);
        assert_eq!(app.changed_files[0].path, "tracked.txt");
        assert_eq!(app.changed_files[0].additions, Some(1));
        assert_eq!(app.changed_files[0].deletions, Some(0));

        let _ = fs::remove_dir_all(repo);
        Ok(())
    }

    #[test]
    fn refresh_app_branch_keeps_cjk_changed_file_paths_readable() -> anyhow::Result<()> {
        let Some(git) = git_command() else {
            return Ok(());
        };
        let repo = unique_temp_dir("yaca-cjk-changed-files-repo");
        fs::create_dir_all(&repo)?;
        run_git(&git, &repo, &["init"])?;
        fs::write(repo.join("临时视觉验证文件.rs"), "fn main() {}\n")?;
        let mut app = AppState::default();
        app.projection.session.workdir = Some(repo.to_string_lossy().into_owned());

        // Given: a git worktree has an untracked CJK filename.
        refresh_app_branch(&mut app);

        // Then: the context rail state keeps the readable Unicode path for ratatui width handling.
        assert!(
            app.changed_files
                .iter()
                .any(|file| file.path == "临时视觉验证文件.rs"),
            "changed files should include the readable CJK path, got {:?}",
            app.changed_files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
        );

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
