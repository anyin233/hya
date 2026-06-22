use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::process::Command;

#[derive(Serialize)]
pub(super) struct Info {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    directory: String,
}

struct Entry {
    path: String,
    branch: Option<String>,
}

pub(super) async fn list(source: &Path) -> Result<Vec<String>, String> {
    if !is_git_source(source).await {
        return Ok(Vec::new());
    }
    let primary = canonical_text(source);
    Ok(entries(source)
        .await?
        .into_iter()
        .filter_map(|entry| {
            let path = PathBuf::from(&entry.path);
            (canonical_text(&path) != primary).then_some(entry.path)
        })
        .collect())
}

pub(super) async fn create(source: &Path, requested: Option<&str>) -> Result<Info, String> {
    ensure_git_source(source).await?;
    let slug = requested.map_or_else(fallback_name, |name| {
        let slug = slugify(name);
        if slug.is_empty() {
            fallback_name()
        } else {
            slug
        }
    });
    let directory = worktree_root(source).join(&slug);
    if directory.exists() {
        return Err(format!(
            "Worktree destination already exists: {}",
            directory.to_string_lossy()
        ));
    }
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Worktree directory unavailable: {} ({e})",
                parent.to_string_lossy()
            )
        })?;
    }
    let branch = format!("opencode/{slug}");
    let directory_text = directory.to_string_lossy().into_owned();
    run_git(
        source,
        vec![
            "worktree".to_string(),
            "add".to_string(),
            "-b".to_string(),
            branch.clone(),
            directory_text.clone(),
            "HEAD".to_string(),
        ],
    )
    .await?;
    Ok(Info {
        name: slug,
        branch: Some(branch),
        directory: directory_text,
    })
}

pub(super) async fn remove(source: &Path, directory: &str) -> Result<bool, String> {
    ensure_git_source(source).await?;
    let Some(entry) = locate(source, directory).await? else {
        return Ok(true);
    };
    run_git(
        source,
        vec![
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            entry.path.clone(),
        ],
    )
    .await?;
    if let Some(branch) = entry.branch.and_then(|branch| branch_name(&branch)) {
        run_git(source, vec!["branch".to_string(), "-D".to_string(), branch]).await?;
    }
    Ok(true)
}

pub(super) async fn reset(source: &Path, directory: &str) -> Result<bool, String> {
    ensure_git_source(source).await?;
    if canonical_text(Path::new(directory)) == canonical_text(source) {
        return Err("Cannot reset the primary workspace".to_string());
    }
    let entry = locate(source, directory)
        .await?
        .ok_or_else(|| "Worktree not found".to_string())?;
    let head = run_git(source, vec!["rev-parse".to_string(), "HEAD".to_string()]).await?;
    run_git(
        Path::new(&entry.path),
        vec![
            "reset".to_string(),
            "--hard".to_string(),
            head.trim().to_string(),
        ],
    )
    .await?;
    run_git(
        Path::new(&entry.path),
        vec!["clean".to_string(), "-ffdx".to_string()],
    )
    .await?;
    Ok(true)
}

async fn ensure_git_source(source: &Path) -> Result<(), String> {
    if !is_git_source(source).await {
        return Err("Worktrees are only supported for git projects".to_string());
    }
    Ok(())
}

async fn is_git_source(source: &Path) -> bool {
    source.exists()
        && run_git(
            source,
            vec!["rev-parse".to_string(), "--is-inside-work-tree".to_string()],
        )
        .await
        .is_ok()
}

async fn entries(source: &Path) -> Result<Vec<Entry>, String> {
    let text = run_git(
        source,
        vec![
            "worktree".to_string(),
            "list".to_string(),
            "--porcelain".to_string(),
        ],
    )
    .await?;
    Ok(parse_entries(&text))
}

async fn locate(source: &Path, directory: &str) -> Result<Option<Entry>, String> {
    let requested = canonical_text(Path::new(directory));
    Ok(entries(source)
        .await?
        .into_iter()
        .find(|entry| canonical_text(Path::new(&entry.path)) == requested))
}

fn parse_entries(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Entry {
                path: path.trim().to_string(),
                branch: None,
            });
        } else if let Some(branch) = line.strip_prefix("branch ")
            && let Some(entry) = out.last_mut()
        {
            entry.branch = Some(branch.trim().to_string());
        }
    }
    out
}

async fn run_git(cwd: &Path, args: Vec<String>) -> Result<String, String> {
    let output = Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !output.status.success() {
        return Err(git_error(&args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_error(args: &[String], output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if !detail.is_empty() {
        return format!("git {args:?} failed: {detail}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stdout.trim();
    if !detail.is_empty() {
        return format!("git {args:?} failed: {detail}");
    }
    format!("git {args:?} failed")
}

fn worktree_root(source: &Path) -> PathBuf {
    let project = source
        .file_name()
        .and_then(|value| value.to_str())
        .map_or_else(|| "project".to_string(), slugify);
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".yaca-worktrees")
        .join(project)
}

fn branch_name(ref_name: &str) -> Option<String> {
    ref_name
        .strip_prefix("refs/heads/")
        .map(ToString::to_string)
        .or_else(|| (!ref_name.is_empty()).then(|| ref_name.to_string()))
}

fn canonical_text(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

fn fallback_name() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    format!("worktree-{millis}-{}", std::process::id())
}
