use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::ApiError;

mod default_branch;
mod patch;
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

pub(in crate::compat) fn branch(workdir: &Path) -> Option<String> {
    output(workdir, &["branch", "--show-current"])
}

pub(in crate::compat) fn default_branch(workdir: &Path) -> Option<String> {
    default_branch::get(workdir)
}

pub(super) fn is_repo(workdir: &Path) -> bool {
    output(workdir, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}

pub(super) fn status(workdir: &Path) -> Result<Vec<FileStatus>, ApiError> {
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

pub(super) fn diff(
    workdir: &Path,
    mode: &str,
    context: Option<usize>,
) -> Result<Vec<FileDiff>, ApiError> {
    let (ref_name, items) = diff_items(workdir, mode)?;
    let ref_name = ref_name.as_deref();
    let mut total_patch_bytes = 0;
    let mut out = Vec::new();
    for item in items {
        let (additions, deletions) = stats(workdir, &item, ref_name)?;
        let patch = patch::for_item(workdir, &item, ref_name, context, total_patch_bytes)?;
        total_patch_bytes = total_patch_bytes.saturating_add(patch.len());
        out.push(FileDiff {
            patch,
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
        chunks.push(patch::untracked_raw(workdir, &item.file)?);
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

fn diff_items(workdir: &Path, mode: &str) -> Result<(Option<String>, Vec<GitItem>), ApiError> {
    match mode {
        "git" => Ok((
            has_head(workdir).then(|| "HEAD".to_string()),
            status::items(workdir)?,
        )),
        "branch" => branch_items(workdir),
        _ => Err(ApiError::bad_request("invalid vcs diff mode")),
    }
}

fn branch_items(workdir: &Path) -> Result<(Option<String>, Vec<GitItem>), ApiError> {
    let Some(default) = default_branch(workdir) else {
        return Ok((None, Vec::new()));
    };
    if branch(workdir).as_deref() == Some(default.as_str()) {
        return Ok((None, Vec::new()));
    }
    let origin_ref = format!("origin/{default}");
    let ref_name = if ref_exists(workdir, &origin_ref) {
        origin_ref
    } else {
        default
    };
    let Some(base) = output(workdir, &["merge-base", "HEAD", &ref_name]) else {
        return Ok((None, Vec::new()));
    };
    let out = text(workdir, &["diff", "--name-status", "-z", &base])?;
    let mut items = items_from_name_status(&out);
    items.extend(
        status::items(workdir)?
            .into_iter()
            .filter(|item| item.code == "??"),
    );
    items.sort_by(|a, b| a.file.cmp(&b.file));
    Ok((Some(base), items))
}

fn items_from_name_status(out: &str) -> Vec<GitItem> {
    out.split('\0')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|chunk| {
            let [code, file] = chunk else {
                return None;
            };
            Some(GitItem {
                file: (*file).to_string(),
                code: (*code).to_string(),
                status: status_name(code),
            })
        })
        .collect()
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

fn ref_exists(workdir: &Path, ref_name: &str) -> bool {
    output(workdir, &["rev-parse", "--verify", ref_name]).is_some()
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
