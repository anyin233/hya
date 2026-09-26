//! Durable pre-change file snapshots for session revert (`FilesChanged`).
//!
//! Before a file-changing tool runs, the engine captures the prior content of
//! the files it may change; after the call it compares, stores the prior
//! content of every file that actually changed as a per-session
//! content-addressed blob (`SessionStore::put_file_blob`), and appends one
//! `FilesChanged` event naming the blobs.
//!
//! Coverage:
//! - `write`, `edit`: the `path` (`file_path`) argument.
//! - `apply_patch`: the `*** Add/Delete/Update File:` and `*** Move to:`
//!   headers of the patch text.
//! - `bash` (model-issued and the user's `!` commands): only when the session
//!   workdir is inside a git work tree. `git status` before and after the
//!   command finds the files it touched (tracked or untracked, never ignored);
//!   a file that was clean before is read back from the `HEAD` tree, a dirty
//!   one from the in-memory capture taken before the command. Paths under the
//!   workdir's `.hya/` (tool artifacts) are skipped.
//!
//! Limits (no unbounded growth): a file larger than [`MAX_FILE_BYTES`] is
//! recorded as `omitted` (`too_large`); a session keeps at most
//! [`MAX_SESSION_BLOB_BYTES`] of blobs (`session_cap` beyond); the bash
//! pre-capture reads at most [`MAX_DIRTY_FILES`] dirty files and
//! [`MAX_DIRTY_BYTES`] in total (`snapshot_budget` beyond). Blobs are
//! deduplicated by hash within a session and deleted with it. A capture
//! failure never fails the tool call: it only loses the snapshot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use hya_proto::{ActorClaim, Event, FileChange, FileState, MessageId, SessionId, ToolCallId};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::SessionEngine;
use crate::error::CoreError;

/// Largest file whose content is kept for revert.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Largest total of kept contents per session.
pub const MAX_SESSION_BLOB_BYTES: u64 = 256 * 1024 * 1024;
/// Most dirty files the bash pre-capture reads.
pub const MAX_DIRTY_FILES: usize = 2_000;
/// Most bytes the bash pre-capture reads in total.
pub const MAX_DIRTY_BYTES: u64 = 16 * 1024 * 1024;
/// Bound on each git call of the bash capture.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Size and modification time: tells an unread (omitted) file changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    len: u64,
    modified: Option<SystemTime>,
}

/// A file's state captured before a call, held in memory until it ends.
#[derive(Clone, Debug)]
pub(crate) enum Prior {
    Absent,
    /// Exists but is not a regular file (directory, socket, …): not tracked.
    NotFile,
    Content(Vec<u8>),
    Omitted {
        signature: Signature,
        reason: &'static str,
    },
}

/// What a file-changing call may touch, captured before it runs.
#[derive(Debug, Default)]
pub(crate) enum FileCapture {
    /// The call changes no files this engine tracks.
    #[default]
    None,
    /// Known target paths (write / edit / apply_patch).
    Paths(Vec<(PathBuf, Prior)>),
    /// A bash command inside a git work tree.
    Git(Box<GitCapture>),
}

#[derive(Debug)]
pub(crate) struct GitCapture {
    root: PathBuf,
    tree: Option<String>,
    dirty: BTreeMap<String, Prior>,
    skip: Option<String>,
}

/// Capture the state the call `tool` (canonical name) with `input` may
/// change, relative to `workdir`.
pub(crate) async fn capture_before(tool: &str, input: &Value, workdir: &Path) -> FileCapture {
    match tool {
        "write" | "edit" | "apply_patch" | "patch" => {
            let paths = target_paths(tool, input, workdir);
            if paths.is_empty() {
                return FileCapture::None;
            }
            let mut captured = Vec::with_capacity(paths.len());
            for path in paths {
                let prior = read_prior(&path).await;
                captured.push((path, prior));
            }
            FileCapture::Paths(captured)
        }
        "bash" | "shell" => match GitCapture::before(workdir).await {
            Some(capture) => FileCapture::Git(Box::new(capture)),
            None => FileCapture::None,
        },
        _ => FileCapture::None,
    }
}

/// Paths a write / edit / apply_patch call targets, resolved like the tools
/// resolve them (lexically against the workdir).
fn target_paths(tool: &str, input: &Value, workdir: &Path) -> Vec<PathBuf> {
    let text = |key: &str| input.get(key).and_then(Value::as_str);
    match tool {
        "write" | "edit" => text("path")
            .or_else(|| text("file_path"))
            // `local://` and other handles are not workdir files.
            .filter(|path| !path.contains("://"))
            .map(|path| vec![resolve(workdir, path)])
            .unwrap_or_default(),
        _ => {
            let Some(patch) = text("patchText").or_else(|| text("patch")) else {
                return Vec::new();
            };
            let mut paths = BTreeSet::new();
            for line in patch.lines() {
                let line = line.trim();
                for prefix in [
                    "*** Add File:",
                    "*** Delete File:",
                    "*** Update File:",
                    "*** Move to:",
                ] {
                    if let Some(path) = line.strip_prefix(prefix) {
                        let path = path.trim();
                        if !path.is_empty() {
                            paths.insert(resolve(workdir, path));
                        }
                    }
                }
            }
            paths.into_iter().collect()
        }
    }
}

/// Lexical resolution against the workdir (no symlink resolution), matching
/// the base tools.
fn resolve(workdir: &Path, path: &str) -> PathBuf {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workdir.join(raw)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn signature(metadata: &std::fs::Metadata) -> Signature {
    Signature {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

async fn read_prior(path: &Path) -> Prior {
    read_prior_within(path, MAX_FILE_BYTES).await
}

async fn read_prior_within(path: &Path, limit: u64) -> Prior {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Prior::Absent,
        Err(_) => {
            return Prior::Omitted {
                signature: Signature {
                    len: 0,
                    modified: None,
                },
                reason: "unreadable",
            };
        }
    };
    if !metadata.is_file() {
        return Prior::NotFile;
    }
    if metadata.len() > limit {
        return Prior::Omitted {
            signature: signature(&metadata),
            reason: if metadata.len() > MAX_FILE_BYTES {
                "too_large"
            } else {
                "snapshot_budget"
            },
        };
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => Prior::Content(bytes),
        Err(_) => Prior::Omitted {
            signature: signature(&metadata),
            reason: "unreadable",
        },
    }
}

/// Whether the file at `path` differs from `prior`.
async fn changed_since(path: &Path, prior: &Prior) -> bool {
    let metadata = tokio::fs::metadata(path).await.ok();
    match (prior, metadata) {
        (Prior::Absent, None) => false,
        (Prior::Absent, Some(metadata)) => metadata.is_file(),
        (Prior::NotFile, metadata) => metadata.is_some_and(|m| m.is_file()),
        (Prior::Content(_) | Prior::Omitted { .. }, None) => true,
        (Prior::Content(bytes), Some(metadata)) => {
            if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
                return true;
            }
            tokio::fs::read(path)
                .await
                .map_or(true, |current| current != *bytes)
        }
        (
            Prior::Omitted {
                signature: before, ..
            },
            Some(metadata),
        ) => !metadata.is_file() || signature(&metadata) != *before,
    }
}

/// Lowercase hex sha256 of `bytes` (the blob key).
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn git(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let run = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Never take the index lock the user's own git may need.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(GIT_TIMEOUT, run).await.ok()?.ok()?;
    output.status.success().then_some(output.stdout)
}

fn split_nul(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
}

impl GitCapture {
    async fn before(workdir: &Path) -> Option<Self> {
        let top = git(workdir, &["rev-parse", "--show-toplevel"]).await?;
        let root = PathBuf::from(String::from_utf8_lossy(&top).trim());
        if root.as_os_str().is_empty() {
            return None;
        }
        // Tool artifacts under `<workdir>/.hya/` are the engine's own files.
        // `--show-toplevel` is canonical; the workdir may not be.
        let skip = [
            workdir.to_path_buf(),
            workdir
                .canonicalize()
                .unwrap_or_else(|_| workdir.to_path_buf()),
        ]
        .iter()
        .find_map(|base| {
            base.join(".hya")
                .strip_prefix(&root)
                .ok()
                .map(|rel| format!("{}/", rel.to_string_lossy()))
        });
        let tree = head_tree(&root).await;
        let mut capture = Self {
            root,
            tree,
            dirty: BTreeMap::new(),
            skip,
        };
        let mut files = 0usize;
        let mut bytes = 0u64;
        for rel in capture.status().await? {
            let path = capture.root.join(&rel);
            let budget_left = files < MAX_DIRTY_FILES && bytes < MAX_DIRTY_BYTES;
            let limit = if budget_left {
                MAX_FILE_BYTES.min(MAX_DIRTY_BYTES - bytes)
            } else {
                0
            };
            let prior = read_prior_within(&path, limit).await;
            if let Prior::Content(content) = &prior {
                files += 1;
                bytes += content.len() as u64;
            }
            capture.dirty.insert(rel, prior);
        }
        Some(capture)
    }

    fn skipped(&self, rel: &str) -> bool {
        self.skip
            .as_deref()
            .is_some_and(|skip| rel.starts_with(skip))
    }

    /// Dirty paths (modified, added, deleted, untracked; never ignored),
    /// relative to the work tree root.
    async fn status(&self) -> Option<Vec<String>> {
        let out = git(
            &self.root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--no-renames",
            ],
        )
        .await?;
        Some(
            split_nul(&out)
                .filter_map(|entry| entry.get(3..).map(str::to_owned))
                .filter(|rel| !self.skipped(rel))
                .collect(),
        )
    }

    /// Paths the command changed, each with its prior state.
    async fn after(self) -> Vec<(PathBuf, Prior)> {
        let Some(dirty_after) = self.status().await else {
            return Vec::new();
        };
        let mut candidates: BTreeSet<String> = self.dirty.keys().cloned().collect();
        candidates.extend(dirty_after);
        let tree_after = head_tree(&self.root).await;
        if tree_after != self.tree {
            // The command moved HEAD (commit, checkout, reset): every path
            // that differs between the two trees is a candidate too.
            let listed = match (&self.tree, &tree_after) {
                (Some(before), Some(after)) => {
                    git(
                        &self.root,
                        &[
                            "diff-tree",
                            "-r",
                            "--name-only",
                            "-z",
                            "--no-renames",
                            before,
                            after,
                        ],
                    )
                    .await
                }
                (None, Some(after)) => {
                    git(&self.root, &["ls-tree", "-r", "--name-only", "-z", after]).await
                }
                (Some(before), None) => {
                    git(&self.root, &["ls-tree", "-r", "--name-only", "-z", before]).await
                }
                (None, None) => None,
            };
            if let Some(listed) = listed {
                candidates.extend(split_nul(&listed).filter(|rel| !self.skipped(rel)));
            }
        }
        let mut changed = Vec::new();
        for rel in candidates {
            let path = self.root.join(&rel);
            let prior = match self.dirty.get(&rel) {
                Some(prior) => prior.clone(),
                None => self.clean_prior(&rel).await,
            };
            if changed_since(&path, &prior).await {
                changed.push((path, prior));
            }
        }
        changed
    }

    /// Prior content of a path that was clean before the command: its blob
    /// in the `HEAD` tree, or absent.
    async fn clean_prior(&self, rel: &str) -> Prior {
        let Some(tree) = &self.tree else {
            return Prior::Absent;
        };
        let spec = format!("{tree}:{rel}");
        let Some(size) = git(&self.root, &["cat-file", "-s", &spec]).await else {
            return Prior::Absent;
        };
        let size: u64 = String::from_utf8_lossy(&size).trim().parse().unwrap_or(0);
        if size > MAX_FILE_BYTES {
            return Prior::Omitted {
                signature: Signature {
                    len: size,
                    modified: None,
                },
                reason: "too_large",
            };
        }
        match git(&self.root, &["cat-file", "blob", &spec]).await {
            Some(bytes) => Prior::Content(bytes),
            None => Prior::Omitted {
                signature: Signature {
                    len: size,
                    modified: None,
                },
                reason: "unreadable",
            },
        }
    }
}

async fn head_tree(root: &Path) -> Option<String> {
    let out = git(root, &["rev-parse", "-q", "--verify", "HEAD^{tree}"]).await?;
    let tree = String::from_utf8_lossy(&out).trim().to_owned();
    (!tree.is_empty()).then_some(tree)
}

/// Running total of a session's blobs, so the cap is checked without a
/// query per file.
pub(crate) struct BlobBudget {
    used: u64,
}

impl SessionEngine {
    pub(crate) async fn blob_budget(&self, session: SessionId) -> Result<BlobBudget, CoreError> {
        Ok(BlobBudget {
            used: self.store.file_blob_bytes(session).await?,
        })
    }

    /// Keep `bytes` as a session blob unless the session cap is reached.
    pub(crate) async fn keep_content(
        &self,
        session: SessionId,
        bytes: &[u8],
        budget: &mut BlobBudget,
    ) -> Result<FileState, CoreError> {
        let size = bytes.len() as u64;
        if budget.used.saturating_add(size) > MAX_SESSION_BLOB_BYTES {
            return Ok(FileState::Omitted {
                size,
                reason: "session_cap".to_string(),
            });
        }
        let hash = content_hash(bytes);
        self.store.put_file_blob(session, &hash, bytes).await?;
        budget.used = budget.used.saturating_add(size);
        Ok(FileState::Stored { hash, size })
    }

    /// The current state of `path`, its content kept as a blob.
    pub(crate) async fn snapshot_path(
        &self,
        session: SessionId,
        path: &Path,
        budget: &mut BlobBudget,
    ) -> Result<FileState, CoreError> {
        match read_prior(path).await {
            Prior::Absent | Prior::NotFile => Ok(FileState::Absent),
            Prior::Content(bytes) => self.keep_content(session, &bytes, budget).await,
            Prior::Omitted { signature, reason } => Ok(FileState::Omitted {
                size: signature.len,
                reason: reason.to_string(),
            }),
        }
    }

    /// After a call ends: record the files it changed (`FilesChanged`).
    pub(crate) async fn record_file_changes(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        message: MessageId,
        call: ToolCallId,
        capture: FileCapture,
    ) -> Result<(), CoreError> {
        let changed = match capture {
            FileCapture::None => return Ok(()),
            FileCapture::Paths(paths) => {
                let mut changed = Vec::new();
                for (path, prior) in paths {
                    if changed_since(&path, &prior).await {
                        changed.push((path, prior));
                    }
                }
                changed
            }
            FileCapture::Git(capture) => capture.after().await,
        };
        if changed.is_empty() {
            return Ok(());
        }
        let mut budget = self.blob_budget(session).await?;
        let mut files = Vec::with_capacity(changed.len());
        for (path, prior) in changed {
            let before = match prior {
                Prior::Absent | Prior::NotFile => FileState::Absent,
                Prior::Content(bytes) => self.keep_content(session, &bytes, &mut budget).await?,
                Prior::Omitted { signature, reason } => FileState::Omitted {
                    size: signature.len,
                    reason: reason.to_string(),
                },
            };
            files.push(FileChange {
                path: path.to_string_lossy().into_owned(),
                before,
            });
        }
        self.emit_for_actor(
            actor_claim,
            session,
            Event::FilesChanged {
                session,
                message,
                call: Some(call),
                files,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_headers_name_every_target() {
        let workdir = Path::new("/w");
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+x\n*** Update File: src/a.rs\n*** Move to: src/b.rs\n@@\n-a\n+b\n*** Delete File: ./old/../gone.txt\n*** End Patch";
        let paths = target_paths("apply_patch", &json!({ "patchText": patch }), workdir);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/w/gone.txt"),
                PathBuf::from("/w/new.txt"),
                PathBuf::from("/w/src/a.rs"),
                PathBuf::from("/w/src/b.rs"),
            ]
        );
    }

    #[test]
    fn write_and_edit_targets_resolve_against_the_workdir() {
        let workdir = Path::new("/w");
        assert_eq!(
            target_paths("write", &json!({"path": "a/../b.txt"}), workdir),
            vec![PathBuf::from("/w/b.txt")]
        );
        assert_eq!(
            target_paths("edit", &json!({"file_path": "/abs/c.txt"}), workdir),
            vec![PathBuf::from("/abs/c.txt")]
        );
        assert!(target_paths("write", &json!({"path": "local://scratch"}), workdir).is_empty());
    }

    #[test]
    fn content_hash_is_lowercase_sha256_hex() {
        assert_eq!(
            content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
