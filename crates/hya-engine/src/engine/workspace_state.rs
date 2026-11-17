//! Read-only workspace identity for request-scoped bundle capabilities.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::file_snapshot::git_output;

/// SHA-256 of the work tree's top-level path, the HEAD tree output,
/// porcelain-v1 status, and status-listed regular-file paths and content
/// digests, excluding top-level `.hya/`. The path is part of the identity
/// because an answer about one checkout can name absolute paths in it.
/// Returns `None` outside a Git work tree, on Git failure/10-second timeout,
/// or when a listed file cannot be read. Index changes can change the identity
/// even when file bytes are unchanged.
pub async fn workspace_state(workdir: &Path) -> Option<String> {
    let top = git_output(workdir, &["rev-parse", "--show-toplevel"]).await?;
    if !top.status.success() {
        return None;
    }
    let root = git_path(top.stdout.strip_suffix(b"\n")?)?;
    let tree = git_output(&root, &["rev-parse", "--verify", "--quiet", "HEAD^{tree}"]).await?;
    let mut hash = Sha256::new();
    hash.update(&top.stdout);
    if tree.status.success() {
        hash.update(&tree.stdout);
    } else {
        // Only an unborn branch means "none"; other failures are unavailable.
        let head = git_output(&root, &["symbolic-ref", "--quiet", "HEAD"]).await?;
        if !head.status.success() {
            return None;
        }
        let reference = std::str::from_utf8(head.stdout.strip_suffix(b"\n")?).ok()?;
        let exists = git_output(&root, &["show-ref", "--verify", "--quiet", reference]).await?;
        if tree.status.code() != Some(1) || exists.status.code() != Some(1) {
            return None;
        }
        hash.update(b"none");
    }
    let status = git_output(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
            ":(top,exclude).hya/**",
        ],
    )
    .await?;
    if !status.status.success() {
        return None;
    }
    hash.update(&status.stdout);
    let mut entries = status.stdout.split(|byte| *byte == 0);
    while let Some(entry) = entries.next() {
        if entry.is_empty() {
            break;
        }
        let path = entry.get(3..)?;
        let renamed = entry[..2].iter().any(|byte| matches!(byte, b'R' | b'C'));
        let source = if renamed { Some(entries.next()?) } else { None };
        // With -z, a rename lists its destination first, then its source.
        for path in std::iter::once(path).chain(source) {
            let absolute = root.join(git_path(path)?);
            let metadata = match tokio::fs::symlink_metadata(&absolute).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            if metadata.is_file() {
                let bytes = tokio::fs::read(&absolute).await.ok()?;
                hash.update(path);
                hash.update([0]);
                hash.update(Sha256::digest(&bytes));
            }
        }
    }
    Some(format!("{:x}", hash.finalize()))
}

fn git_path(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(std::str::from_utf8(bytes).ok()?))
    }
}
