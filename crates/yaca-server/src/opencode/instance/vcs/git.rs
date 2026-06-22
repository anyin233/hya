use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::ApiError;

mod status;

#[derive(Serialize)]
pub(super) struct FileStatus {
    file: String,
    additions: usize,
    deletions: usize,
    status: &'static str,
}

#[derive(Serialize)]
pub(super) struct FileDiff {
    file: String,
    patch: String,
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

pub(super) fn branch(workdir: &Path) -> Option<String> {
    output(workdir, &["branch", "--show-current"])
}

pub(super) fn default_branch(workdir: &Path) -> Option<String> {
    output(
        workdir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .map(|branch| match branch.strip_prefix("origin/") {
        Some(name) => name.to_string(),
        None => branch,
    })
}

pub(super) fn is_repo(workdir: &Path) -> bool {
    output(workdir, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

pub(super) fn status(workdir: &Path) -> Result<Vec<FileStatus>, ApiError> {
    let has_head = has_head(workdir);
    let mut out = Vec::new();
    for item in status::items(workdir)? {
        let (additions, deletions) = stats(workdir, &item, has_head)?;
        out.push(FileStatus {
            file: item.file,
            additions,
            deletions,
            status: item.status,
        });
    }
    Ok(out)
}

pub(super) fn diff(
    workdir: &Path,
    mode: &str,
    context: Option<usize>,
) -> Result<Vec<FileDiff>, ApiError> {
    let items = diff_items(workdir, mode)?;
    let has_head = has_head(workdir);
    let mut out = Vec::new();
    for item in items {
        let (additions, deletions) = stats(workdir, &item, has_head)?;
        out.push(FileDiff {
            patch: patch(workdir, &item, has_head, context)?,
            file: item.file,
            additions,
            deletions,
            status: item.status,
        });
    }
    Ok(out)
}

pub(super) fn raw_diff(workdir: &Path) -> Result<String, ApiError> {
    let mut chunks = Vec::new();
    if has_head(workdir) {
        let tracked = text(workdir, &["diff", "HEAD"])?;
        if !tracked.is_empty() {
            chunks.push(tracked);
        }
    }
    for item in status::items(workdir)?
        .into_iter()
        .filter(|item| item.code == "??")
    {
        chunks.push(patch_untracked(workdir, &item.file)?);
    }
    Ok(chunks.join("\n"))
}

pub(super) fn apply_patch(workdir: &Path, patch: &str) -> Result<(), ()> {
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

fn diff_items(workdir: &Path, mode: &str) -> Result<Vec<GitItem>, ApiError> {
    match mode {
        "git" => status::items(workdir),
        "branch" => branch_items(workdir),
        _ => Err(ApiError::bad_request("invalid vcs diff mode")),
    }
}

fn branch_items(workdir: &Path) -> Result<Vec<GitItem>, ApiError> {
    let Some(default) = default_branch(workdir) else {
        return Ok(Vec::new());
    };
    if branch(workdir).as_deref() == Some(default.as_str()) {
        return Ok(Vec::new());
    }
    let ref_name = format!("origin/{default}");
    let Some(base) = output(workdir, &["merge-base", "HEAD", &ref_name]) else {
        return Ok(Vec::new());
    };
    let out = text(workdir, &["diff", "--name-status", &base])?;
    Ok(out.lines().filter_map(item_from_name_status).collect())
}

fn item_from_name_status(line: &str) -> Option<GitItem> {
    let (code, file) = line.split_once('\t')?;
    Some(GitItem {
        file: file.to_string(),
        code: code.to_string(),
        status: status_name(code),
    })
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

fn stats(workdir: &Path, item: &GitItem, has_head: bool) -> Result<(usize, usize), ApiError> {
    if item.code == "??" || !has_head {
        return Ok((line_count(&workdir.join(&item.file))?, 0));
    }
    let out = text(workdir, &["diff", "--numstat", "HEAD", "--", &item.file])?;
    let Some(line) = out.lines().next() else {
        return Ok((0, 0));
    };
    let mut fields = line.split('\t');
    Ok((parse_usize(fields.next()), parse_usize(fields.next())))
}

fn parse_usize(value: Option<&str>) -> usize {
    value.and_then(|text| text.parse().ok()).unwrap_or(0)
}

fn patch(
    workdir: &Path,
    item: &GitItem,
    has_head: bool,
    context: Option<usize>,
) -> Result<String, ApiError> {
    if item.code == "??" || !has_head {
        return patch_untracked(workdir, &item.file);
    }
    let unified = format!("--unified={}", context.unwrap_or(2_147_483_647));
    text(workdir, &["diff", &unified, "HEAD", "--", &item.file])
}

fn patch_untracked(workdir: &Path, file: &str) -> Result<String, ApiError> {
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
