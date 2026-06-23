use std::path::Path;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ignore;
use super::mime;
use super::path::{
    collect_paths, entry_kind, join_under, matches_kind, relative_path, resolve_existing, workdir,
};
use super::search;
use crate::{ApiError, ServerState};

#[derive(Deserialize)]
pub(super) struct FileQuery {
    path: String,
    directory: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct FindTextQuery {
    pattern: String,
    directory: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct FindFileQuery {
    query: String,
    dirs: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<usize>,
    directory: Option<String>,
}

pub(super) type EmptyQuery = std::collections::BTreeMap<String, String>;

#[derive(Serialize)]
pub(super) struct LegacyEntry {
    name: String,
    path: String,
    absolute: String,
    #[serde(rename = "type")]
    kind: &'static str,
    ignored: bool,
}

#[derive(Serialize)]
pub(super) struct LegacyContent {
    #[serde(rename = "type")]
    kind: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    mime_type: Option<&'static str>,
}

#[derive(Serialize)]
struct TextField {
    text: String,
}

#[derive(Serialize)]
struct LegacySubmatch {
    #[serde(rename = "match")]
    match_text: TextField,
    start: usize,
    end: usize,
}

#[derive(Serialize)]
pub(super) struct LegacyMatch {
    path: TextField,
    lines: TextField,
    line_number: usize,
    absolute_offset: usize,
    submatches: Vec<LegacySubmatch>,
}

pub(super) async fn list(
    State(st): State<ServerState>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<LegacyEntry>>, ApiError> {
    let root = workdir(&st, query.directory.as_deref(), &headers);
    let dir = resolve_existing(&root, &query.path)?;
    let ignore = ignore::IgnoreSet::load(&root);
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
        let path = entry.path();
        entries.push(LegacyEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: relative_path(&root, &path),
            absolute: path.to_string_lossy().into_owned(),
            kind,
            ignored: ignore.matches(&root, &path, kind == "directory"),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(entries))
}

pub(super) async fn content(
    State(st): State<ServerState>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Result<Json<LegacyContent>, ApiError> {
    let root = workdir(&st, query.directory.as_deref(), &headers);
    let path = join_under(&root, &query.path)?;
    if !path.exists() {
        return Ok(Json(text_content(String::new())));
    }
    let path = resolve_existing(&root, &query.path)?;
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(content_from_bytes(bytes)))
}

pub(super) async fn find_text(
    State(st): State<ServerState>,
    Query(query): Query<FindTextQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<LegacyMatch>>, ApiError> {
    let regex = Regex::new(&query.pattern).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let root = workdir(&st, query.directory.as_deref(), &headers);
    let mut files = Vec::new();
    collect_paths(&root, &mut files);
    files.retain(|path| path.is_file());
    files.sort();
    let mut matches = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        collect_text_matches(&regex, &root, &file, &text, &mut matches);
        if matches.len() >= 10 {
            break;
        }
    }
    matches.truncate(10);
    Ok(Json(matches))
}

pub(super) async fn find_file(
    State(st): State<ServerState>,
    Query(query): Query<FindFileQuery>,
    headers: HeaderMap,
) -> Result<Json<Vec<String>>, ApiError> {
    let root = workdir(&st, query.directory.as_deref(), &headers);
    let limit = query.limit.unwrap_or(10);
    if !(1..=200).contains(&limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 200"));
    }
    if !matches!(query.dirs.as_deref(), None | Some("true") | Some("false")) {
        return Err(ApiError::bad_request("dirs must be true or false"));
    }
    if let Some(kind) = query.kind.as_deref()
        && kind != "file"
        && kind != "directory"
    {
        return Err(ApiError::bad_request("type must be file or directory"));
    }
    let kind = query
        .kind
        .as_deref()
        .or_else(|| (query.dirs.as_deref() == Some("false")).then_some("file"));
    let mut paths = Vec::new();
    collect_paths(&root, &mut paths);
    let mut found = search::ranked_paths(
        &root,
        paths.into_iter().filter(|path| matches_kind(path, kind)),
        &query.query,
    )
    .into_iter()
    .map(|path| relative_path(&root, &path))
    .collect::<Vec<_>>();
    found.truncate(limit);
    Ok(Json(found))
}

pub(super) async fn empty_array<T>(Query(_query): Query<T>) -> Json<Vec<Value>> {
    Json(Vec::new())
}

fn collect_text_matches(
    regex: &Regex,
    root: &Path,
    file: &Path,
    text: &str,
    matches: &mut Vec<LegacyMatch>,
) {
    let mut offset = 0;
    for (index, line) in text.lines().enumerate() {
        let submatches = regex
            .find_iter(line)
            .map(|found| LegacySubmatch {
                match_text: TextField {
                    text: found.as_str().to_string(),
                },
                start: found.start(),
                end: found.end(),
            })
            .collect::<Vec<_>>();
        if let Some(first) = submatches.first() {
            matches.push(LegacyMatch {
                path: TextField {
                    text: relative_path(root, file),
                },
                lines: TextField {
                    text: line.to_string(),
                },
                line_number: index + 1,
                absolute_offset: offset + first.start,
                submatches,
            });
        }
        offset += line.len() + 1;
    }
}

fn content_from_bytes(bytes: Vec<u8>) -> LegacyContent {
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => text_content(text.trim().to_string()),
        Ok(text) => binary_content(text.into_bytes()),
        Err(error) => binary_content(error.into_bytes()),
    }
}

fn text_content(content: String) -> LegacyContent {
    LegacyContent {
        kind: "text",
        content,
        encoding: None,
        mime_type: None,
    }
}

fn binary_content(bytes: Vec<u8>) -> LegacyContent {
    let mime = mime::sniff(&bytes);
    LegacyContent {
        kind: "binary",
        content: base64::engine::general_purpose::STANDARD.encode(bytes),
        encoding: Some("base64"),
        mime_type: Some(mime),
    }
}
