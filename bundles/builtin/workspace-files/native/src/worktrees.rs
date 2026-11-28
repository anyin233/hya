use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::process::Command;

use hya_runtime_abi::{HostError, NativeBundleApiRequest};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WriteRequest {
    directory: String,
    name: String,
    // These fields were accepted by the original API; the path identifies
    // the worktree, and create always branches from HEAD.
    #[serde(rename = "branch")]
    _branch: String,
    #[serde(rename = "worktree")]
    _worktree: String,
}

pub(super) async fn call(request: &NativeBundleApiRequest) -> Result<Value, HostError> {
    let body = if request.body.is_null() {
        WriteRequest::default()
    } else {
        serde_json::from_value(request.body.clone())
            .map_err(|error| HostError::InvalidInput(error.to_string()))?
    };
    let directory = match request.api.as_str() {
        "create-worktree" | "reset-worktree" => Some(body.directory.as_str()),
        _ => request.query.get("directory").map(String::as_str),
    };
    let source = super::directory(directory)?;
    let value = match request.api.as_str() {
        "list-worktrees" => {
            let worktrees = infos(source).await.map_err(HostError::Internal)?;
            json!({ "worktrees": worktrees })
        }
        "create-worktree" => {
            let name = (!body.name.is_empty()).then_some(body.name.as_str());
            create(source, name).await.map_err(HostError::Internal)?
        }
        "delete-worktree" => {
            remove(source, &request.path_params["worktree"])
                .await
                .map_err(HostError::Internal)?;
            json!({})
        }
        "reset-worktree" => {
            let id = &request.path_params["worktree"];
            reset(source, id).await.map_err(HostError::Internal)?;
            worktree(id, None)
        }
        _ => unreachable!("the BundleApi router selects a declared worktree endpoint"),
    };
    Ok(value)
}

fn worktree(path: &str, branch: Option<&str>) -> Value {
    let mut value = json!({ "id": path, "path": path });
    if let Some(branch) = branch {
        value["branch"] = json!(branch);
    }
    value
}

struct Entry {
    path: String,
    branch: Option<String>,
}

async fn infos(source: &Path) -> Result<Vec<Value>, String> {
    if !is_git_source(source).await {
        return Ok(Vec::new());
    }
    let primary = canonical_text(source);
    Ok(entries(source)
        .await?
        .into_iter()
        .filter(|entry| {
            let path = PathBuf::from(&entry.path);
            canonical_text(&path) != primary
        })
        .map(|entry| {
            let branch = entry.branch.as_deref().and_then(branch_name);
            worktree(&entry.path, branch.as_deref())
        })
        .collect())
}

async fn create(source: &Path, requested: Option<&str>) -> Result<Value, String> {
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
    let branch = format!("compat/{slug}");
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
    Ok(worktree(&directory_text, Some(&branch)))
}

async fn remove(source: &Path, directory: &str) -> Result<(), String> {
    ensure_git_source(source).await?;
    let Some(entry) = locate(source, directory).await? else {
        return Ok(());
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
    Ok(())
}

async fn reset(source: &Path, directory: &str) -> Result<(), String> {
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
    Ok(())
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
        .join(".hya-worktrees")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_lists_resets_and_removes_a_linked_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        let scratch = tempfile::tempdir()?;
        let source = scratch.path().join("repo");
        std::fs::create_dir(&source)?;
        run_git(&source, vec!["init".into()]).await?;
        run_git(
            &source,
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ]
            .map(str::to_owned)
            .to_vec(),
        )
        .await?;

        let created = create(&source, Some("My feature")).await?;
        let path = created["path"].as_str().ok_or("missing worktree path")?;
        assert_eq!(created["id"], path);
        assert_eq!(created["branch"], "compat/my-feature");
        assert_eq!(infos(&source).await?, vec![created.clone()]);

        let untracked = Path::new(path).join("untracked");
        std::fs::write(&untracked, "discard me")?;
        reset(&source, path).await?;
        assert!(!untracked.exists());

        remove(&source, path).await?;
        assert!(!Path::new(path).exists());
        assert!(infos(&source).await?.is_empty());
        Ok(())
    }
}
