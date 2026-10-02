use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::ApiError;

mod status;

#[derive(Serialize)]
pub(crate) struct FileStatus {
    file: String,
    additions: usize,
    deletions: usize,
    status: &'static str,
}

#[derive(Clone)]
struct GitItem {
    file: String,
    code: String,
    status: &'static str,
}
fn status_name(code: &str) -> &'static str {
    if code.contains('D') {
        "deleted"
    } else if code.contains('A') || code == "??" {
        "added"
    } else {
        "modified"
    }
}

fn untracked_raw(workdir: &Path, file: &str) -> Result<String, ApiError> {
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

pub(crate) fn branch(workdir: &Path) -> Option<String> {
    output(workdir, &["branch", "--show-current"])
}

pub(crate) fn is_repo(workdir: &Path) -> bool {
    output(workdir, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

pub(crate) fn status(workdir: &Path) -> Result<Vec<FileStatus>, ApiError> {
    let ref_name = has_head(workdir).then_some("HEAD");
    let mut out = Vec::new();
    for item in status::items(workdir)? {
        let (additions, deletions) = stats(workdir, &item, ref_name)?;
        out.push(FileStatus {
            file: item.file,
            additions,
            deletions,
            status: item.status,
        });
    }
    Ok(out)
}

/// Unified `git diff HEAD` plus untracked files, restricted to `paths`
/// (git pathspecs relative to `workdir`) when non-empty.
pub(crate) fn raw_diff(workdir: &Path, paths: &[String]) -> Result<String, ApiError> {
    let dot = [".".to_owned()];
    let pathspecs = if paths.is_empty() { &dot[..] } else { paths };
    let mut chunks = Vec::new();
    if has_head(workdir) {
        let mut args = vec!["diff", "HEAD", "--"];
        args.extend(pathspecs.iter().map(String::as_str));
        let tracked = text(workdir, &args)?;
        if !tracked.is_empty() {
            chunks.push(tracked);
        }
    }
    for item in status::items_in(workdir, pathspecs)?
        .into_iter()
        .filter(|item| item.code == "??")
    {
        chunks.push(untracked_raw(workdir, &item.file)?);
    }
    Ok(chunks.join("\n"))
}

pub(crate) fn apply_patch(workdir: &Path, patch: &str) -> Result<(), ()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("apply")
        .arg("--whitespace=nowarn")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| ())?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(());
    };
    stdin.write_all(patch.as_bytes()).map_err(|_| ())?;
    drop(stdin);
    child
        .wait()
        .map_err(|_| ())
        .and_then(|status| status.success().then_some(()).ok_or(()))
}

fn stats(
    workdir: &Path,
    item: &GitItem,
    ref_name: Option<&str>,
) -> Result<(usize, usize), ApiError> {
    let Some(ref_name) = ref_name else {
        return Ok((line_count(&workdir.join(&item.file))?, 0));
    };
    if item.code == "??" {
        return Ok((line_count(&workdir.join(&item.file))?, 0));
    }
    let out = text(workdir, &["diff", "--numstat", ref_name, "--", &item.file])?;
    let Some(line) = out.lines().next() else {
        return Ok((0, 0));
    };
    let mut fields = line.split('\t');
    Ok((parse_usize(fields.next()), parse_usize(fields.next())))
}

fn parse_usize(value: Option<&str>) -> usize {
    value.and_then(|text| text.parse().ok()).unwrap_or(0)
}

fn has_head(workdir: &Path) -> bool {
    match Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
    {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

fn output(workdir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn text(workdir: &Path, args: &[&str]) -> Result<String, ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .output()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if !output.status.success() {
        return Err(ApiError::internal(stderr(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|e| ApiError::internal(e.to_string()))
}

fn stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).into_owned()
}

fn line_count(path: &Path) -> Result<usize, ApiError> {
    if path.is_dir() {
        return Ok(0);
    }
    let text = std::fs::read_to_string(path).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(text.lines().count())
}
