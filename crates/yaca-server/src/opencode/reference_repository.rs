use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

pub(super) struct RepositoryRef {
    host: String,
    segments: Vec<String>,
    remote: String,
}

pub(super) fn parse(input: &str) -> Option<RepositoryRef> {
    let cleaned = normalize(input);
    if cleaned.is_empty() {
        return None;
    }
    if let Some(rest) = cleaned.strip_prefix("github:") {
        let segments = parts(rest);
        return (segments.len() == 2)
            .then(|| build_remote("github.com", segments, None))
            .flatten();
    }
    if !cleaned.contains("://") {
        if let Some((left, right)) = cleaned.split_once(':')
            && !left.contains('/')
            && !right.is_empty()
        {
            let host = left.rsplit('@').next().unwrap_or(left);
            return build_remote(host, parts(right), Some(cleaned.clone()));
        }
        let direct = parts(&cleaned);
        if direct.len() >= 2 && host_like(&direct[0]) {
            return build_remote(&direct[0], direct[1..].to_vec(), None);
        }
        if direct.len() == 2 {
            return build_remote("github.com", direct, None);
        }
    }
    let (scheme, rest) = cleaned.split_once("://")?;
    if scheme == "file" {
        return None;
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let segments = parts(path);
    let remote = if host.eq_ignore_ascii_case("github.com") {
        Some(github_remote(&segments.join("/")))
    } else {
        Some(cleaned.clone())
    };
    build_remote(host, segments, remote)
}

pub(super) fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && branch
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '.' | '-'))
}

pub(super) fn cache_path(reference: &RepositoryRef) -> PathBuf {
    let mut path = repos_root();
    for part in reference.host.split(':').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    for segment in &reference.segments {
        path.push(segment);
    }
    path
}

pub(super) fn materialize(repository: &str, branch: Option<&str>, path: PathBuf) {
    let Some(reference) = parse(repository) else {
        return;
    };
    if path.join(".git").is_dir() || !mark_active(&path) {
        return;
    }
    let remote = reference.remote;
    let branch = branch.map(ToString::to_string);
    tokio::spawn(async move {
        if let Err(error) = ensure(&remote, branch.as_deref(), &path).await {
            tracing::warn!(
                %error,
                repository = %remote,
                path = %path.display(),
                "failed to materialize opencode reference"
            );
        }
        unmark_active(&path);
    });
}

fn normalize(input: &str) -> String {
    let mut value = input
        .trim()
        .strip_prefix("git+")
        .unwrap_or(input.trim())
        .to_string();
    if let Some((before, _)) = value.split_once('#') {
        value = before.to_string();
    }
    while value.ends_with('/') {
        value.pop();
    }
    value
}

fn parts(input: &str) -> Vec<String> {
    input
        .split('/')
        .map(str::trim)
        .map(trim_git_suffix)
        .filter(|item| !item.is_empty())
        .collect()
}

fn trim_git_suffix(input: &str) -> String {
    input.strip_suffix(".git").unwrap_or(input).to_string()
}

fn build_remote(
    host: &str,
    segments: Vec<String>,
    remote: Option<String>,
) -> Option<RepositoryRef> {
    let host = host.to_ascii_lowercase();
    if !safe_host(&host)
        || segments.is_empty()
        || segments.iter().any(|segment| !safe_segment(segment))
    {
        return None;
    }
    let repository_path = segments.join("/");
    let remote = remote.unwrap_or_else(|| default_remote(&host, &repository_path));
    Some(RepositoryRef {
        host,
        segments,
        remote,
    })
}

fn safe_host(input: &str) -> bool {
    !input.is_empty()
        && !input.starts_with('-')
        && !input
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '\\'))
}

fn safe_segment(input: &str) -> bool {
    input != "."
        && input != ".."
        && !input
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '\\' | ':'))
}

fn host_like(input: &str) -> bool {
    input.contains('.') || input.contains(':') || input == "localhost"
}

fn repos_root() -> PathBuf {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("opencode").join("repos");
    }
    home_dir()
        .map(|home| home.join(".local/share/opencode/repos"))
        .unwrap_or_else(|| PathBuf::from(".local/share/opencode/repos"))
}

fn active_paths() -> &'static Mutex<BTreeSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn mark_active(path: &Path) -> bool {
    let Ok(mut active) = active_paths().lock() else {
        return false;
    };
    active.insert(path.to_path_buf())
}

fn unmark_active(path: &Path) {
    if let Ok(mut active) = active_paths().lock() {
        active.remove(path);
    }
}

async fn ensure(remote: &str, branch: Option<&str>, path: &Path) -> Result<(), String> {
    if path.join(".git").is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    if path.exists() {
        remove_existing(path)?;
    }
    let mut command = Command::new("git");
    command.arg("clone").arg("--depth").arg("1");
    if let Some(branch) = branch {
        command.arg("--branch").arg(branch);
    }
    command.arg(remote).arg(path).kill_on_drop(true);
    let output = timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "git clone timed out".to_string())?
        .map_err(|error| error.to_string())?;
    output.status.success().then_some(()).ok_or_else(|| {
        let stderr = output_text(&output.stderr);
        if stderr.is_empty() {
            "git clone failed".to_string()
        } else {
            stderr
        }
    })
}

fn remove_existing(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        std::fs::remove_file(path).map_err(|error| error.to_string())
    }
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn default_remote(host: &str, path: &str) -> String {
    if host == "github.com" {
        return github_remote(path);
    }
    format!("https://{host}/{path}.git")
}

fn github_remote(path: &str) -> String {
    std::env::var("OPENCODE_REPO_CLONE_GITHUB_BASE_URL").map_or_else(
        |_| format!("https://github.com/{path}.git"),
        |base| format!("{}/{}.git", base.trim_end_matches('/'), path),
    )
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
