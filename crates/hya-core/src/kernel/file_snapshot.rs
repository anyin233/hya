//! Durable pre-change file snapshots for session revert (`FilesChanged`).
//!
//! Before a file-changing tool runs, the kernel captures the prior content of
//! the files it may change; after the call it compares, stores the prior
//! content of every file that actually changed as a per-session
//! content-addressed blob (`SessionStore::put_file_blob`), and appends one
//! `FilesChanged` event naming the blobs.
//!
//! Coverage:
//! - `write`, `edit`: the `path` (`file_path`) argument.
//! - `apply_patch`: the `*** Add/Delete/Update File:` and `*** Move to:`
//!   headers of the patch text.
//! - `bash` (model-issued and the user's `!` commands): only when the command
//!   workdir is inside a Git work tree. `git status` before and after the
//!   command finds the files it touched (tracked or untracked, never ignored);
//!   a file that was clean before is read back from the `HEAD` tree, a dirty
//!   one from the in-memory capture taken before the command. Paths under the
//!   workdir's `.hya/` (tool artifacts) are skipped.
//!   A completed shell samples workspace changes as its own observations
//!   (`FilesObserved { external: false }`) independently of Git revert capture.
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_proto::event::FileStamp;
use hya_proto::{Event, FileChange, FileState, MessageId, SessionId, ToolCallId};
use hya_store::SessionStore;
use serde_json::Value;
use sha2::{Digest, Sha256};

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
struct Signature {
    len: u64,
    modified: Option<SystemTime>,
}

/// A file's state captured before a call, held in memory until it ends.
#[derive(Clone, Debug)]
enum Prior {
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
enum Capture {
    /// The call changes no files this engine tracks.
    #[default]
    None,
    /// A read of an explicit path or a grep result with structured paths.
    Read {
        path: Option<PathBuf>,
        grep_workdir: Option<PathBuf>,
    },
    /// Known target paths (write / edit / apply_patch).
    Paths(Vec<(PathBuf, Prior)>),
    /// A bash command; `Some` inside a git work tree.
    Shell(Option<Box<GitCapture>>),
}

#[derive(Debug)]
struct GitCapture {
    root: PathBuf,
    tree: Option<String>,
    dirty: BTreeMap<String, Prior>,
    skip: Option<String>,
}

/// The paths a tool may read or change, with pre-call content for revert.
#[derive(Debug, Default)]
pub struct FileCapture(Capture);

impl FileCapture {
    /// Capture a bare tool's reads or potential writes before execution.
    pub(super) async fn bare(tool: &str, input: &Value, workdir: &Path) -> Self {
        let path = || text(input, "path").map(|path| super::tools::resolve(workdir, path));
        match tool {
            "read" => Self(Capture::Read {
                path: path(),
                grep_workdir: None,
            }),
            "write" | "edit" => Self::paths(path()).await,
            "bash" => {
                if text(input, "command").is_none() {
                    return Self::default();
                }
                Self::git(workdir, workdir).await
            }
            _ => Self::default(),
        }
    }

    /// Capture a full-mode tool's reads or potential writes before execution.
    pub async fn full(tool: &str, input: &Value, workdir: &Path) -> Self {
        match tool {
            "read" | "lsp" | "grep" => {
                let path = text(input, "path")
                    .filter(|path| !path.is_empty())
                    .or_else(|| text(input, "filePath"))
                    .filter(|path| !path.contains("://"))
                    .map(|path| resolve(workdir, path));
                Self(Capture::Read {
                    path,
                    grep_workdir: (tool == "grep").then(|| workdir.to_path_buf()),
                })
            }
            "write" | "edit" | "apply_patch" | "patch" => {
                Self::paths(target_paths(tool, input, workdir)).await
            }
            "bash" | "shell" if text(input, "command").is_some() => {
                let cwd = text(input, "cwd").map(|cwd| resolve(workdir, cwd));
                Self::git(cwd.as_deref().unwrap_or(workdir), workdir).await
            }
            _ => Self::default(),
        }
    }

    async fn paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut captured = Vec::new();
        for path in paths {
            let prior = read_prior(&path).await;
            captured.push((path, prior));
        }
        Self(Capture::Paths(captured))
    }

    async fn git(cwd: &Path, workdir: &Path) -> Self {
        Self(Capture::Shell(
            GitCapture::before(cwd, workdir).await.map(Box::new),
        ))
    }

    /// Return the revert snapshot and stamps once the tool has ended.
    /// A failed mutation can still leave changed files behind.
    pub async fn finish(
        self,
        store: &SessionStore,
        session: SessionId,
        message: MessageId,
        call: ToolCallId,
        output: Option<&Value>,
        workspace: (&super::WorkspaceWatches, &Path),
    ) -> Result<(Option<Event>, Vec<FileStamp>), CoreError> {
        if matches!(self.0, Capture::None) {
            return Ok((None, Vec::new()));
        }
        let (watches, workdir) = workspace;
        let mut watch = if matches!(self.0, Capture::Shell(_)) {
            watches
                .changes(
                    store,
                    session,
                    store.read_projection_shared(session).await?,
                    BTreeMap::new(),
                    false,
                )
                .await
        } else {
            watches.tool_cut(workdir, session).await
        };
        let (event, stamps) = self
            .finish_at(
                store,
                session,
                message,
                call,
                output,
                std::mem::take(&mut watch.files),
            )
            .await?;
        watch.tool_observed(&stamps);
        Ok((event, stamps))
    }

    async fn finish_at(
        self,
        store: &SessionStore,
        session: SessionId,
        message: MessageId,
        call: ToolCallId,
        output: Option<&Value>,
        shell_changes: Vec<FileStamp>,
    ) -> Result<(Option<Event>, Vec<FileStamp>), CoreError> {
        let mut stamps = Vec::new();
        let (changed, observed) = match self.0 {
            Capture::None => return Ok((None, stamps)),
            Capture::Read { path, grep_workdir } => {
                if let Some(output) = output {
                    let mut paths: BTreeSet<PathBuf> = path.into_iter().collect();
                    if let Some(workdir) = grep_workdir
                        && let Some(groups) = output
                            .pointer("/metadata/display/groups")
                            .and_then(Value::as_array)
                    {
                        paths.extend(
                            groups
                                .iter()
                                .filter_map(|group| text(group, "path"))
                                .map(|path| resolve(&workdir, path)),
                        );
                    }
                    for path in paths {
                        if let Some(file) = stamp(&path).await
                            && file.size.is_some()
                        {
                            stamps.push(file);
                        }
                    }
                }
                return Ok((None, stamps));
            }
            Capture::Paths(paths) => {
                let mut changed = Vec::new();
                for (path, prior) in paths {
                    let changed_here = changed_since(&path, &prior).await;
                    if (changed_here || output.is_some())
                        && let Some(file) = stamp(&path).await
                    {
                        stamps.push(file);
                    }
                    if changed_here {
                        changed.push((path, prior));
                    }
                }
                (changed, false)
            }
            Capture::Shell(capture) => {
                let changed = match capture {
                    Some(capture) => capture.after().await,
                    None => Vec::new(),
                };
                for (path, _) in &changed {
                    if let Some(file) = stamp(path).await {
                        stamps.push(file);
                    }
                }
                let captured: BTreeSet<_> = stamps.iter().map(|s| s.path.clone()).collect();
                stamps.extend(
                    shell_changes
                        .into_iter()
                        .filter(|s| !captured.contains(&s.path)),
                );
                (changed, true)
            }
        };
        if changed.is_empty() {
            return Ok((None, stamps));
        }
        let mut budget = BlobBudget::load(store, session).await?;
        let mut files = Vec::with_capacity(changed.len());
        for (path, prior) in changed {
            files.push(FileChange {
                path: path.to_string_lossy().into_owned(),
                before: budget.keep_prior(store, session, prior).await?,
                observed,
            });
        }
        Ok((
            Some(Event::FilesChanged {
                session,
                message,
                call: Some(call),
                files,
            }),
            stamps,
        ))
    }
}

fn text<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// Observe a path after tool execution or before the next request.
async fn stamp(path: &Path) -> Option<FileStamp> {
    let path = std::path::absolute(path).ok()?;
    stamp_metadata(&path, tokio::fs::metadata(&path).await)
}

#[cfg(target_os = "linux")]
pub(super) fn stamp_sync(path: &Path) -> Option<FileStamp> {
    stamp_metadata(path, std::fs::metadata(path))
}

pub(super) fn stamp_metadata(
    path: &Path,
    meta: std::io::Result<std::fs::Metadata>,
) -> Option<FileStamp> {
    let (mtime_ns, size) = match meta {
        Ok(metadata) if metadata.is_file() => {
            let modified = metadata.modified().ok()?;
            let nanos = match modified.duration_since(UNIX_EPOCH) {
                Ok(duration) => duration.as_nanos() as i128,
                Err(error) => -(error.duration().as_nanos() as i128),
            };
            (Some(i64::try_from(nanos).ok()?), Some(metadata.len()))
        }
        Ok(_) => (None, None),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            (None, None)
        }
        Err(_) => return None,
    };
    Some(FileStamp {
        path: path.to_string_lossy().into_owned(),
        mtime_ns,
        size,
    })
}

/// Return paths whose on-disk metadata differs from the current projection.
pub(super) async fn external_changes(known: &BTreeMap<String, FileStamp>) -> Vec<FileStamp> {
    let mut changes = Vec::new();
    for (path, before) in known {
        if let Some(after) = stamp(Path::new(path)).await
            && after != *before
        {
            changes.push(after);
        }
    }
    changes
}

/// A nonempty batch of stamps for the session log.
pub fn observation(session: SessionId, external: bool, files: Vec<FileStamp>) -> Option<Event> {
    (!files.is_empty()).then_some(Event::FilesObserved {
        session,
        external,
        files,
    })
}

/// Paths a write / edit / apply_patch call targets, resolved like the tools
/// resolve them (lexically against the workdir).
fn target_paths(tool: &str, input: &Value, workdir: &Path) -> Vec<PathBuf> {
    let text = |key| text(input, key);
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
fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn git(cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = git_output(cwd, args).await?;
    output.status.success().then_some(output.stdout)
}

/// Run an observational Git command without taking the index lock.
async fn git_output(cwd: &Path, args: &[&str]) -> Option<std::process::Output> {
    let run = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        // Never take the index lock the user's own git may need.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(GIT_TIMEOUT, run).await.ok()?.ok()
}

fn split_nul(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
}

impl GitCapture {
    async fn before(cwd: &Path, workdir: &Path) -> Option<Self> {
        let top = git(cwd, &["rev-parse", "--show-toplevel"]).await?;
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
pub struct BlobBudget {
    used: u64,
}

impl BlobBudget {
    /// Load the current size of the session's stored file contents.
    pub async fn load(store: &SessionStore, session: SessionId) -> Result<Self, CoreError> {
        Ok(Self {
            used: store.file_blob_bytes(session).await?,
        })
    }

    /// The current state of `path`, its content kept as a blob.
    pub async fn snapshot_path(
        &mut self,
        store: &SessionStore,
        session: SessionId,
        path: &Path,
    ) -> Result<FileState, CoreError> {
        self.keep_prior(store, session, read_prior(path).await)
            .await
    }

    async fn keep_prior(
        &mut self,
        store: &SessionStore,
        session: SessionId,
        prior: Prior,
    ) -> Result<FileState, CoreError> {
        let bytes = match prior {
            Prior::Absent | Prior::NotFile => return Ok(FileState::Absent),
            Prior::Content(bytes) => bytes,
            Prior::Omitted { signature, reason } => {
                return Ok(FileState::Omitted {
                    size: signature.len,
                    reason: reason.to_string(),
                });
            }
        };
        let size = bytes.len() as u64;
        if self.used.saturating_add(size) > MAX_SESSION_BLOB_BYTES {
            return Ok(FileState::Omitted {
                size,
                reason: "session_cap".to_string(),
            });
        }
        let hash = content_hash(&bytes);
        // Blobs stay unreferenced until the corresponding event commits.
        store.put_file_blob(session, &hash, &bytes).await?;
        self.used = self.used.saturating_add(size);
        Ok(FileState::Stored { hash, size })
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
