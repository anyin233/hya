use std::path::Path;

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiError, ServerState};

mod ignore;
mod mime;
mod path;

use path::{
    collect_paths, entry_kind, join_under, matches_kind, relative_path, resolve_existing, workdir,
};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/find", get(find_text))
        .route("/find/file", get(find_file))
        .route("/find/symbol", get(empty_array::<FindSymbolQuery>))
        .route("/file", get(list))
        .route("/file/content", get(content))
        .route("/file/status", get(empty_array::<StatusQuery>))
}

#[derive(Deserialize)]
struct FileQuery {
    path: String,
}

#[derive(Deserialize)]
struct FindTextQuery {
    pattern: String,
}

#[derive(Deserialize)]
struct FindFileQuery {
    query: String,
    dirs: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct FindSymbolQuery {
    #[allow(dead_code)]
    query: String,
}

#[derive(Deserialize)]
struct StatusQuery {}

#[derive(Serialize)]
struct LegacyEntry {
    name: String,
    path: String,
    absolute: String,
    #[serde(rename = "type")]
    kind: &'static str,
    ignored: bool,
}

#[derive(Serialize)]
struct LegacyContent {
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
struct LegacyMatch {
    path: TextField,
    lines: TextField,
    line_number: usize,
    absolute_offset: usize,
    submatches: Vec<LegacySubmatch>,
}

async fn list(
    State(st): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<Vec<LegacyEntry>>, ApiError> {
    let root = workdir(&st);
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

async fn content(
    State(st): State<ServerState>,
    Query(query): Query<FileQuery>,
) -> Result<Json<LegacyContent>, ApiError> {
    let root = workdir(&st);
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

async fn find_text(
    State(st): State<ServerState>,
    Query(query): Query<FindTextQuery>,
) -> Result<Json<Vec<LegacyMatch>>, ApiError> {
    let regex = Regex::new(&query.pattern).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let root = workdir(&st);
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

async fn find_file(
    State(st): State<ServerState>,
    Query(query): Query<FindFileQuery>,
) -> Result<Json<Vec<String>>, ApiError> {
    let root = workdir(&st);
    let limit = query.limit.unwrap_or(10).clamp(1, 200);
    let kind = query
        .kind
        .as_deref()
        .or_else(|| (query.dirs.as_deref() == Some("false")).then_some("file"));
    let needle = query.query.to_ascii_lowercase();
    let mut paths = Vec::new();
    collect_paths(&root, &mut paths);
    let mut found = paths
        .into_iter()
        .filter(|path| matches_kind(path, kind))
        .filter(|path| {
            relative_path(&root, path)
                .to_ascii_lowercase()
                .contains(&needle)
        })
        .map(|path| relative_path(&root, &path))
        .collect::<Vec<_>>();
    found.sort();
    found.truncate(limit);
    Ok(Json(found))
}

async fn empty_array<T>(Query(_query): Query<T>) -> Json<Vec<Value>> {
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
