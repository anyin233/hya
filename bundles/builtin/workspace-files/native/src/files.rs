use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use base64::Engine as _;
use hya_runtime_abi::HostError;
use serde::Serialize;
use serde_json::{Value, json};

type Query = BTreeMap<String, String>;

fn text<'a>(query: &'a Query, key: &str) -> &'a str {
    query.get(key).map_or("", String::as_str)
}

fn number<T: Default + FromStr>(query: &Query, key: &str) -> Result<T, HostError> {
    let value = text(query, key);
    if value.is_empty() {
        return Ok(T::default());
    }
    value
        .parse()
        .map_err(|_| HostError::InvalidInput(format!("invalid numeric parameter `{key}`: {value}")))
}

fn root(query: &Query) -> Result<&Path, HostError> {
    super::directory(query.get("directory").map(String::as_str))
}

fn limit(query: &Query, default: u32, cap: u32) -> Result<usize, HostError> {
    let n = number::<u32>(query, "limit")?;
    Ok(if n == 0 { default } else { n.min(cap) } as usize)
}

pub(super) async fn read(query: &Query) -> Result<Value, HostError> {
    let path = resolve_under(root(query)?, text(query, "path"))?;
    let cap = number::<u64>(
        query,
        if query.contains_key("maxBytes") {
            "maxBytes"
        } else {
            "max_bytes"
        },
    )?;
    let bytes = read_capped(&path, cap)
        .await
        .map_err(|error| HostError::InvalidInput(error.to_string()))?;
    Ok(json!({
        "mime": mime_guess::from_path(&path).first_or_octet_stream().to_string(),
        "text": std::str::from_utf8(&bytes).is_ok(),
        "content": base64::engine::general_purpose::STANDARD.encode(bytes),
    }))
}

pub(super) async fn list(query: &Query) -> Result<Value, HostError> {
    let dir = resolve_under(root(query)?, text(query, "path"))?;
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir)
        .await
        .map_err(|error| HostError::InvalidInput(error.to_string()))?;
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|error| HostError::Internal(error.to_string()))?
    {
        let metadata = entry
            .metadata()
            .await
            .map_err(|error| HostError::Internal(error.to_string()))?;
        let kind = if metadata.is_dir() {
            "DIR_ENTRY_KIND_DIRECTORY"
        } else if metadata.is_symlink() {
            "DIR_ENTRY_KIND_SYMLINK"
        } else {
            "DIR_ENTRY_KIND_FILE"
        };
        let millis = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_millis() as i64);
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size: metadata.len().to_string(),
            time_modified: pbjson_types::Timestamp {
                seconds: millis.div_euclid(1000),
                nanos: (millis.rem_euclid(1000) * 1_000_000) as i32,
            },
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(json!({ "entries": entries }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryEntry {
    name: String,
    kind: &'static str,
    size: String,
    time_modified: pbjson_types::Timestamp,
}

pub(super) fn find(query: &Query) -> Result<Value, HostError> {
    let root = root(query)?;
    let limit = limit(query, 500, 2000)?;
    let matcher = GlobMatcher::new(text(query, "pattern"))?;
    let mut paths = Vec::new();
    walk(root, &mut |path| {
        if paths.len() >= limit {
            return WalkControl::Stop;
        }
        let relative = path.strip_prefix(root).unwrap_or(path);
        if matcher.matches(&relative.to_string_lossy()) {
            paths.push(relative.to_string_lossy().into_owned());
        }
        WalkControl::Continue
    });
    paths.sort();
    Ok(json!({ "paths": paths }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TextMatch {
    path: String,
    line_number: u32,
    text: String,
}

pub(super) fn search_text(query: &Query) -> Result<Value, HostError> {
    let root = root(query)?;
    let limit = limit(query, 200, 1000)?;
    let glob = text(query, "glob");
    let glob = (!glob.is_empty())
        .then(|| GlobMatcher::new(glob))
        .transpose()?;
    let needle = text(query, "query");
    let mut matches = Vec::new();
    walk(root, &mut |path| {
        if matches.len() >= limit {
            return WalkControl::Stop;
        }
        let relative = path.strip_prefix(root).unwrap_or(path);
        let relative = relative.to_string_lossy().into_owned();
        if let Some(glob) = &glob
            && !glob.matches(&relative)
        {
            return WalkControl::Continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            for (index, line) in content.lines().enumerate() {
                if line.contains(needle) {
                    matches.push(TextMatch {
                        path: relative.clone(),
                        line_number: (index + 1) as u32,
                        text: line.chars().take(400).collect(),
                    });
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }
        WalkControl::Continue
    });
    matches.sort_by(|a, b| a.path.cmp(&b.path).then(a.line_number.cmp(&b.line_number)));
    Ok(json!({ "matches": matches }))
}

/// Resolve `path` under `root`, rejecting traversal outside it.
fn resolve_under(root: &Path, path: &str) -> Result<PathBuf, HostError> {
    let joined = if path.is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|_| HostError::InvalidInput(format!("path not found: {path}")))?;
    let root = root
        .canonicalize()
        .map_err(|error| HostError::Internal(error.to_string()))?;
    if canonical.starts_with(&root) {
        Ok(canonical)
    } else {
        Err(HostError::InvalidInput(
            "path escapes the directory scope".into(),
        ))
    }
}

/// The file's bytes, at most `max_bytes` of them (0 = the whole file).
async fn read_capped(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;
    if max_bytes == 0 {
        return tokio::fs::read(path).await;
    }
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(max_bytes).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

enum WalkControl {
    Continue,
    Stop,
}

/// Bounded recursive walk skipping VCS/build directories.
fn walk(root: &Path, visit: &mut dyn FnMut(&Path) -> WalkControl) {
    fn recurse(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path) -> WalkControl) -> bool {
        if depth > 12 {
            return true;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return true;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = path.is_dir();
            if is_dir && matches!(name.as_str(), ".git" | "target" | "node_modules" | ".cache") {
                continue;
            }
            if matches!(visit(&path), WalkControl::Stop) {
                return false;
            }
            if is_dir && !recurse(&path, depth + 1, visit) {
                return false;
            }
        }
        true
    }
    recurse(root, 0, visit);
}

/// Small glob matcher translating `*`, `?`, and `**` to regex alternation.
struct GlobMatcher {
    regex: regex::Regex,
}

impl GlobMatcher {
    fn new(pattern: &str) -> Result<Self, HostError> {
        let mut escaped = String::from("^");
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' => {
                    if chars.peek() == Some(&'*') {
                        chars.next();
                        let _: Option<char> = chars.next(); // consume '/'
                        escaped.push_str("(.*/)?");
                    } else {
                        escaped.push_str("[^/]*");
                    }
                }
                '?' => escaped.push_str("[^/]"),
                other => {
                    if regex::escape(&other.to_string()).len() > 1 {
                        escaped.push_str(&regex::escape(&other.to_string()));
                    } else {
                        escaped.push(other);
                    }
                }
            }
        }
        escaped.push('$');
        let regex = regex::Regex::new(&escaped)
            .map_err(|error| HostError::InvalidInput(format!("invalid glob: {error}")))?;
        Ok(Self { regex })
    }

    fn matches(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_file_operations_keep_caps_paths_and_wire_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let scratch = tempfile::tempdir()?;
        let root = scratch.path().join("scope");
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/a.rs"), "first\nneedle\n")?;
        std::fs::write(root.join("binary.png"), [0xff, 0x00, 0x01, 0x02])?;
        std::fs::write(scratch.path().join("outside"), "outside the scope")?;

        let query = |params: &[(&str, &str)]| -> Query {
            std::iter::once(("directory".to_owned(), root.to_string_lossy().into_owned()))
                .chain(params.iter().map(|(k, v)| ((*k).into(), (*v).into())))
                .collect()
        };
        let file = read(&query(&[("path", "binary.png"), ("maxBytes", "2")])).await?;
        assert_eq!(
            file,
            json!({ "content": "/wA=", "text": false, "mime": "image/png" })
        );
        assert!(matches!(
            read(&query(&[("path", "../outside")])).await,
            Err(HostError::InvalidInput(message)) if message.contains("escapes the directory scope")
        ));
        let entries = list(&query(&[])).await?;
        assert_eq!(entries["entries"][0]["name"], "binary.png");
        assert_eq!(entries["entries"][0]["size"], "4");
        assert_eq!(entries["entries"][1]["kind"], "DIR_ENTRY_KIND_DIRECTORY");
        assert_eq!(
            find(&query(&[("pattern", "**/*.rs")]))?,
            json!({ "paths": ["src/a.rs"] })
        );
        assert_eq!(
            search_text(&query(&[("query", "needle"), ("glob", "**/*.rs")]))?,
            json!({ "matches": [{ "path": "src/a.rs", "lineNumber": 2, "text": "needle" }] })
        );
        Ok(())
    }
}
