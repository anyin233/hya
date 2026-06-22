use std::path::Path;
use std::process::Command;

use crate::ApiError;

use super::{GitItem, stderr, text};

const PATCH_CONTEXT_LINES: usize = 2_147_483_647;
const MAX_PATCH_BYTES: usize = 10_000_000;

pub(super) fn for_item(
    workdir: &Path,
    item: &GitItem,
    has_head: bool,
    context: Option<usize>,
) -> Result<String, ApiError> {
    let patch = if item.code == "??" || !has_head {
        untracked_raw(workdir, &item.file)?
    } else {
        let unified = format!("--unified={}", context.unwrap_or(PATCH_CONTEXT_LINES));
        text(workdir, &["diff", &unified, "HEAD", "--", &item.file])?
    };
    Ok(cap_patch(&item.file, patch))
}

pub(super) fn untracked_raw(workdir: &Path, file: &str) -> Result<String, ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("diff")
        .arg("--no-index")
        .arg("--")
        .arg("/dev/null")
        .arg(workdir.join(file))
        .output()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if matches!(output.status.code(), Some(0) | Some(1)) {
        return String::from_utf8(output.stdout).map_err(|e| ApiError::internal(e.to_string()));
    }
    Err(ApiError::internal(stderr(&output.stderr)))
}

fn cap_patch(file: &str, patch: String) -> String {
    if patch.is_empty() || patch.len() > MAX_PATCH_BYTES {
        empty_patch(file)
    } else {
        patch
    }
}

fn empty_patch(file: &str) -> String {
    format!(
        "Index: {file}\n===================================================================\n--- {file}\n+++ {file}\n"
    )
}
