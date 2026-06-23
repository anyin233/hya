use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::IntoResponse;
use serde::Serialize;

use crate::{ApiError, ServerState};

use super::mime;
use super::path::{collect_paths, entry_kind, matches_kind, relative_path, resolve_existing};
use super::search;
use crate::opencode::location::{LocationRef, LocationResponse};

#[derive(Serialize)]
pub(super) struct Entry {
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
    mime: String,
}

type FsQuery = BTreeMap<String, String>;

pub(super) async fn read(
    State(st): State<ServerState>,
    AxumPath(path): AxumPath<String>,
    Query(query): Query<FsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let location = LocationRef::from_request(&query, &headers);
    let root = crate::opencode::location::workdir_at(&st, &location);
    let path = resolve_existing(&root, &path)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut headers = HeaderMap::new();
    let mime = mime_for_path(&path, Some(&bytes));
    let content_type =
        HeaderValue::from_str(mime.as_ref()).map_err(|e| ApiError::internal(e.to_string()))?;
    headers.insert(header::CONTENT_TYPE, content_type);
    Ok((headers, bytes))
}

pub(super) async fn list(
    State(st): State<ServerState>,
    Query(query): Query<FsQuery>,
    headers: HeaderMap,
) -> Result<Json<LocationResponse<Vec<Entry>>>, ApiError> {
    let location = LocationRef::from_request(&query, &headers);
    let root = crate::opencode::location::workdir_at(&st, &location);
    let path = query.get("path").map_or(".", String::as_str);
    let dir = resolve_existing(&root, path)?;
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        let Some(kind) = entry_kind(file_type.is_dir(), file_type.is_file()) else {
            continue;
        };
        entries.push(fs_entry(&root, entry.path(), kind));
    }
    entries.sort_by(|a, b| match (a.kind, b.kind) {
        ("directory", "file") => std::cmp::Ordering::Less,
        ("file", "directory") => std::cmp::Ordering::Greater,
        _ => a.path.cmp(&b.path),
    });
    Ok(Json(crate::opencode::location::response_at(
        &st, &location, entries,
    )))
}

pub(super) async fn find(
    State(st): State<ServerState>,
    Query(query): Query<FsQuery>,
    headers: HeaderMap,
) -> Result<Json<LocationResponse<Vec<Entry>>>, ApiError> {
    let location = LocationRef::from_request(&query, &headers);
    let root = crate::opencode::location::workdir_at(&st, &location);
    let kind = query.get("type").map(String::as_str);
    if let Some(kind) = kind
        && kind != "file"
        && kind != "directory"
    {
        return Err(ApiError::bad_request("type must be file or directory"));
    }
    let limit = match query.get("limit") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be positive"))?,
        None => 50,
    };
    if limit == 0 {
        return Err(ApiError::bad_request("limit must be positive"));
    }
    let Some(needle) = query.get("query") else {
        return Err(ApiError::bad_request("missing query"));
    };
    let mut paths = Vec::new();
    collect_paths(&root, &mut paths);
    let mut entries = search::ranked_paths(
        &root,
        paths.into_iter().filter(|path| matches_kind(path, kind)),
        needle,
    )
    .into_iter()
    .filter_map(|path| fs_entry_for_path(&root, path))
    .collect::<Vec<_>>();
    entries.truncate(limit.min(200));
    Ok(Json(crate::opencode::location::response_at(
        &st, &location, entries,
    )))
}

fn fs_entry(root: &Path, path: PathBuf, kind: &'static str) -> Entry {
    let mut relative = relative_path(root, &path);
    if kind == "directory" && !relative.ends_with('/') {
        relative.push('/');
    }
    Entry {
        path: relative,
        kind,
        mime: if kind == "directory" {
            "application/x-directory".to_string()
        } else {
            mime_for_path(&path, None).into_owned()
        },
    }
}

fn fs_entry_for_path(root: &Path, path: PathBuf) -> Option<Entry> {
    let kind = entry_kind(path.is_dir(), path.is_file())?;
    Some(fs_entry(root, path, kind))
}

fn mime_for_path(path: &Path, bytes: Option<&[u8]>) -> Cow<'static, str> {
    if let Some(mime) = explicit_extension_mime(path) {
        return Cow::Borrowed(mime);
    }
    if let Some(mime) = mime_guess::from_path(path).first_raw() {
        return Cow::Borrowed(mime);
    }
    bytes.map_or(Cow::Borrowed("application/octet-stream"), |bytes| {
        Cow::Borrowed(mime::sniff(bytes))
    })
}

fn explicit_extension_mime(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt" | "rs" | "toml" | "yaml" | "yml" | "ts" | "tsx" | "js" | "jsx" | "css")
        | Some("html" | "py" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "sh") => {
            Some("text/plain")
        }
        Some("csv") => Some("text/csv"),
        Some("md") => Some("text/markdown"),
        Some("json") => Some("application/json"),
        Some("xml") => Some("application/xml"),
        Some("svg") => Some("image/svg+xml"),
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("ico") => Some("image/x-icon"),
        Some("pdf") => Some("application/pdf"),
        Some("wasm") => Some("application/wasm"),
        Some("mp3") => Some("audio/mpeg"),
        Some("wav") => Some("audio/wav"),
        Some("ogg") => Some("audio/ogg"),
        Some("mp4") => Some("video/mp4"),
        Some("webm") => Some("video/webm"),
        _ => None,
    }
}
