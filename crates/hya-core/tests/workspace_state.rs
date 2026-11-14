//! Workspace identity for request-scoped bundle capabilities.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hya_core::engine::workspace_state;
use hya_proto::SessionId;
use sha2::{Digest, Sha256};

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("hya-workspace-state-{}", SessionId::new()));
        std::fs::create_dir(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "--quiet", "--template=", "--initial-branch=main"]);
        repo.git(&["config", "user.name", "Workspace test"]);
        repo.git(&["config", "user.email", "workspace@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo.git(&["config", "status.renames", "true"]);
        repo
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        output.stdout
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(path), bytes).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
    }

    async fn state(&self) -> String {
        let state = workspace_state(&self.0).await.unwrap();
        assert_eq!(state.len(), 64);
        assert!(
            state
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        state
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn tracked_untracked_and_committed_changes_affect_state() {
    let repo = Repo::new();
    repo.write("tracked", b"initial");
    repo.commit("initial");
    let initial = repo.state().await;
    assert_eq!(initial, repo.state().await);

    repo.write("tracked", b"modified");
    let modified = repo.state().await;
    assert_ne!(initial, modified);
    repo.write("tracked", b"modified again");
    assert_ne!(modified, repo.state().await);
    repo.write("tracked", b"initial");
    assert_eq!(initial, repo.state().await);

    repo.write("new file\nwith newline", b"new");
    let added = repo.state().await;
    assert_ne!(initial, added);
    repo.write("new file\nwith newline", b"changed");
    assert_ne!(added, repo.state().await);
    repo.commit("changed tree");
    assert_ne!(initial, repo.state().await);
}

#[tokio::test]
async fn ignored_and_top_level_hya_files_do_not_affect_state() {
    let repo = Repo::new();
    repo.write(".gitignore", b"ignored\n");
    repo.commit("initial");
    std::fs::create_dir(repo.0.join("subdir")).unwrap();
    let initial = repo.state().await;
    repo.write("ignored", b"one");
    assert_eq!(initial, repo.state().await);
    repo.write("ignored", b"two");
    assert_eq!(initial, repo.state().await);
    std::fs::create_dir(repo.0.join(".hya")).unwrap();
    repo.write(".hya/state", b"one");
    assert_eq!(initial, repo.state().await);
    repo.write(".hya/state", b"two");
    assert_eq!(initial, repo.state().await);
    assert_eq!(
        Some(initial.clone()),
        workspace_state(&repo.0.join("subdir")).await
    );
    std::fs::create_dir(repo.0.join("subdir/.hya")).unwrap();
    repo.write("subdir/.hya/state", b"included");
    assert_ne!(initial, repo.state().await);
}

#[tokio::test]
async fn commit_metadata_does_not_affect_state() {
    let repo = Repo::new();
    repo.write("tracked", b"content");
    repo.commit("first message");
    let initial = repo.state().await;
    let first_commit = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&[
        "commit",
        "--quiet",
        "--amend",
        "-m",
        "different message",
        "--date=2000-01-01T00:00:00Z",
    ]);
    assert_ne!(first_commit, repo.git(&["rev-parse", "HEAD"]));
    assert_eq!(initial, repo.state().await);
}

#[tokio::test]
async fn identical_repositories_have_identical_states() {
    let first = Repo::new();
    let second = Repo::new();
    for repo in [&first, &second] {
        repo.write("tracked", b"same content");
        repo.commit("initial");
        repo.write("tracked", b"same dirty content");
        repo.write("untracked", b"same untracked content");
    }
    assert_eq!(first.state().await, second.state().await);
}

#[tokio::test]
async fn unborn_repository_uses_literal_none_and_raw_file_digest() {
    let repo = Repo::new();
    repo.write("loose.txt", b"content");
    let mut expected = Sha256::new();
    expected.update(b"none");
    expected.update(b"?? loose.txt\0");
    expected.update(b"loose.txt\0");
    expected.update(Sha256::digest(b"content"));
    assert_eq!(repo.state().await, format!("{:x}", expected.finalize()));
}

#[tokio::test]
async fn index_changes_can_conservatively_change_state() {
    let repo = Repo::new();
    repo.write("tracked", b"initial");
    repo.commit("initial");
    repo.write("tracked", b"modified");
    let unstaged = repo.state().await;
    repo.git(&["add", "tracked"]);
    assert_ne!(unstaged, repo.state().await);
}

#[tokio::test]
async fn renamed_paths_are_nul_delimited_and_hashed() {
    let repo = Repo::new();
    repo.write("old name", b"initial");
    repo.commit("initial");
    repo.git(&["mv", "old name", "new\nname"]);
    let renamed = repo.state().await;
    repo.write("new\nname", b"modified");
    assert_ne!(renamed, repo.state().await);
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_paths_keep_their_raw_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let repo = Repo::new();
    let path = Path::new(std::ffi::OsStr::from_bytes(b"file-\xff"));
    std::fs::write(repo.0.join(path), b"initial").unwrap();
    let initial = repo.state().await;
    std::fs::write(repo.0.join(path), b"modified").unwrap();
    assert_ne!(initial, repo.state().await);
}

#[tokio::test]
async fn outside_worktree_and_git_failures_return_none() {
    let repo = Repo::new();
    repo.write("tracked", b"initial");
    repo.commit("initial");
    repo.write(".git/index", b"broken index");
    assert_eq!(workspace_state(&repo.0).await, None);
    std::fs::remove_dir_all(repo.0.join(".git")).unwrap();
    assert_eq!(workspace_state(&repo.0).await, None);
}
