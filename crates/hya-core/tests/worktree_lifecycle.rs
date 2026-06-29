#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_core::{TmuxPaneManager, WorktreeManager};
use hya_proto::TeamRunId;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-wt-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

#[tokio::test]
#[ignore = "requires the git binary"]
async fn worktree_create_edit_dirty_cleanup_owned_only() {
    let repo = tempdir();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t.local"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), "hi").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    let mgr = WorktreeManager::new(&repo);
    let team = TeamRunId::new();

    let worktree = mgr.allocate(team, "alice", "HEAD").await.unwrap();
    assert!(worktree.exists());
    assert!(
        !mgr.dirty(&worktree).await.unwrap(),
        "fresh worktree is clean"
    );

    std::fs::write(worktree.join("change.txt"), "edited").unwrap();
    assert!(
        mgr.dirty(&worktree).await.unwrap(),
        "edited worktree is dirty"
    );
    assert_eq!(mgr.owned_paths(team).len(), 1);

    mgr.release_all(team).await.unwrap();
    assert!(!worktree.exists(), "owned worktree is cleaned");
    assert_eq!(mgr.owned_paths(team).len(), 0);
    // the main checkout is untouched
    assert!(repo.join("README.md").exists());
}

#[tokio::test]
async fn tmux_capability_probe_and_degrade() {
    let available = TmuxPaneManager::available().await;
    if !available {
        let mgr = TmuxPaneManager::new("hya-test");
        let result = mgr.open(Path::new("/tmp"), "true").await;
        assert!(result.is_err(), "tmux open must error when tmux is absent");
    }
}
