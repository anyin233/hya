//! `/v1` filesystem domain: read, list, find, text search, and symbol
//! search under a directory scope.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;

use super::Json;
use serde_json::Value;

use crate::ServerState;
use hya_api::v1 as pb;

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/fs/read", get(read_file))
        .route("/v1/fs/list", get(list_directory))
        .route("/v1/fs/find", get(find_files))
        .route("/v1/fs/search", get(search_text))
        .route("/v1/fs/symbols", get(search_symbols))
}

/// Resolve `path` under `root`, rejecting traversal outside it.
fn resolve_under(root: &Path, path: &str) -> Result<PathBuf, V1Error> {
    let joined = if path.is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|_| V1Error::invalid_argument(format!("path not found: {path}")))?;
    let root = root
        .canonicalize()
        .map_err(|error| V1Error::internal(error.to_string()))?;
    if canonical.starts_with(&root) {
        Ok(canonical)
    } else {
        Err(V1Error::invalid_argument(
            "path escapes the directory scope",
        ))
    }
}

async fn read_file(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ReadFileResponse>, V1Error> {
    let request: pb::ReadFileRequest = super::query_request(&[], &query)?;
    let root = scope_directory(&headers, &request.directory)?;
    let path = resolve_under(&root, &request.path)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| V1Error::invalid_argument(error.to_string()))?;
    let text = std::str::from_utf8(&bytes).is_ok();
    Ok(Json(pb::ReadFileResponse {
        mime: mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string(),
        text,
        content: bytes,
    }))
}

async fn list_directory(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListDirectoryResponse>, V1Error> {
    let request: pb::ListDirectoryRequest = super::query_request(&[], &query)?;
    let root = scope_directory(&headers, &request.directory)?;
    let dir = resolve_under(&root, &request.path)?;
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir)
        .await
        .map_err(|error| V1Error::invalid_argument(error.to_string()))?;
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|error| V1Error::internal(error.to_string()))?
    {
        let metadata = entry
            .metadata()
            .await
            .map_err(|error| V1Error::internal(error.to_string()))?;
        let kind = if metadata.is_dir() {
            pb::DirEntryKind::Directory as i32
        } else if metadata.is_symlink() {
            pb::DirEntryKind::Symlink as i32
        } else {
            pb::DirEntryKind::File as i32
        };
        entries.push(pb::DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            size: metadata.len(),
            time_modified: super::convert::timestamp(
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_millis() as i64),
            ),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(pb::ListDirectoryResponse { entries }))
}

async fn find_files(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::FindFilesResponse>, V1Error> {
    let request: pb::FindFilesRequest = super::query_request(&[], &query)?;
    let root = scope_directory(&headers, &request.directory)?;
    let limit = if request.limit == 0 {
        500
    } else {
        request.limit.min(2000)
    } as usize;
    let matcher = GlobMatcher::new(&request.pattern)?;
    let mut paths = Vec::new();
    walk(&root, &mut |path| {
        if paths.len() >= limit {
            return WalkControl::Stop;
        }
        let relative = path.strip_prefix(&root).unwrap_or(path);
        if matcher.matches(&relative.to_string_lossy()) {
            paths.push(relative.to_string_lossy().into_owned());
        }
        WalkControl::Continue
    });
    paths.sort();
    Ok(Json(pb::FindFilesResponse { paths }))
}

async fn search_text(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::SearchTextResponse>, V1Error> {
    let request: pb::SearchTextRequest = super::query_request(&[], &query)?;
    let root = scope_directory(&headers, &request.directory)?;
    let limit = if request.limit == 0 {
        200
    } else {
        request.limit.min(1000)
    } as usize;
    let glob = if request.glob.is_empty() {
        None
    } else {
        Some(GlobMatcher::new(&request.glob)?)
    };
    let mut matches = Vec::new();
    walk(&root, &mut |path| {
        if matches.len() >= limit {
            return WalkControl::Stop;
        }
        let relative = path.strip_prefix(&root).unwrap_or(path);
        let relative = relative.to_string_lossy().into_owned();
        if let Some(glob) = &glob
            && !glob.matches(&relative)
        {
            return WalkControl::Continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            for (index, line) in content.lines().enumerate() {
                if line.contains(request.query.as_str()) {
                    matches.push(pb::TextMatch {
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
    Ok(Json(pb::SearchTextResponse { matches }))
}

async fn search_symbols(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::SearchSymbolsResponse>, V1Error> {
    let request: pb::SearchSymbolsRequest = super::query_request(&[], &query)?;
    let root = scope_directory(&headers, &request.directory)?;
    let symbols = st
        .engine
        .lsp()
        .workspace_symbols(&root, request.query.clone())
        .await
        .unwrap_or_default();
    let limit = if request.limit == 0 {
        10
    } else {
        request.limit.min(100)
    } as usize;
    let mut mapped: Vec<pb::Symbol> = symbols
        .iter()
        .take(limit)
        .map(|symbol| pb::Symbol {
            path: field(symbol, "path"),
            name: field(symbol, "name"),
            kind: symbol_kind(symbol),
            line_number: symbol
                .get("line")
                .or_else(|| symbol.pointer("location").and_then(|l| l.get("line")))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
        })
        .collect();
    mapped.truncate(limit);
    Ok(Json(pb::SearchSymbolsResponse { symbols: mapped }))
}

fn symbol_kind(symbol: &Value) -> i32 {
    match symbol.get("kind").and_then(Value::as_u64) {
        Some(5) | Some(6) => pb::SymbolKind::Function as i32,
        Some(10) | Some(11) | Some(12) | Some(13) | Some(14) | Some(23) => {
            pb::SymbolKind::Type as i32
        }
        Some(2) | Some(3) => pb::SymbolKind::Module as i32,
        _ => pb::SymbolKind::Other as i32,
    }
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
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
    fn new(pattern: &str) -> Result<Self, V1Error> {
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
            .map_err(|error| V1Error::invalid_argument(format!("invalid glob: {error}")))?;
        Ok(Self { regex })
    }

    fn matches(&self, path: &str) -> bool {
        self.regex.is_match(path)
    }
}
