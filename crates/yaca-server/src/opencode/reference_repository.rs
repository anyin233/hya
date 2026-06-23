use std::path::PathBuf;

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

pub(super) fn remote(reference: &RepositoryRef) -> &str {
    &reference.remote
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
