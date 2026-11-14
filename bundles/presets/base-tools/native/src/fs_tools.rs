use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::ToolSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::grep;
use crate::lsp_path::{display_path, resolve_file};
use hya_tool::tool::obj_schema;
use hya_tool::{Action, ProjectScope, Resource, Tool, ToolCtx, ToolError, glob_match};

const SEARCH_LIMIT: usize = 100;
const SEARCH_BUDGET: Duration = Duration::from_secs(30);
const MAX_GLOB_BYTES: usize = 4096;

/// Visit non-directory entries breadth-first without following directory symlinks.
fn walk(
    root: &Path,
    cancel: &CancellationToken,
    deadline: Instant,
    mut visit: impl FnMut(PathBuf),
) -> Result<bool, ToolError> {
    let mut directories = VecDeque::from([root.to_path_buf()]);
    while let Some(directory) = directories.pop_front() {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries {
            if cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push_back(entry.path());
            } else {
                visit(entry.path());
            }
        }
    }
    Ok(true)
}

fn relative_title(path: &Path, workdir: &Path) -> String {
    let relative = path.strip_prefix(workdir).unwrap_or(path);
    let title = relative.to_string_lossy().replace('\\', "/");
    if title.is_empty() {
        ".".to_string()
    } else {
        title
    }
}

/// Require `ExternalDirectory` permission when `target` lies outside every
/// Project root (ADR-0026).
///
/// Containment is judged by [`ProjectScope`] after symlink resolution. The
/// asked resource names the canonical target directory itself when
/// `is_directory`, otherwise the canonical directory the target lives in,
/// followed by `/*`. An "allow always" on it covers exactly that directory.
///
/// # Errors
/// Returns the permission plane's denial or unavailability.
pub(crate) async fn assert_external_directory(
    ctx: &ToolCtx,
    target: &Path,
    is_directory: bool,
) -> Result<(), ToolError> {
    ProjectScope::for_ctx(ctx)
        .authorize(&ctx.permission, target, |scope| {
            if is_directory {
                scope.outside_directory_pattern(target)
            } else {
                scope.outside_dir_pattern(target)
            }
        })
        .await?;
    Ok(())
}

/// Authorize an external Grep or Glob target with one kind-blind resource:
/// the canonical directory the target lives in, followed by `/*`.
pub(crate) async fn assert_external_search_target(
    ctx: &ToolCtx,
    target: &Path,
) -> Result<(), ToolError> {
    assert_external_directory(ctx, target, false).await
}

/// Reject a caller-provided Glob pattern that exceeds the native matcher bound.
fn validate_glob_pattern(pattern: &str) -> Result<(), ToolError> {
    if pattern.len() > MAX_GLOB_BYTES {
        return Err(ToolError::Input(format!(
            "glob pattern exceeds {MAX_GLOB_BYTES} bytes"
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
}
/// Recursive path matcher with a 100-row cap (`SEARCH_LIMIT`).
pub struct GlobTool;
#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "glob",
            "List files under a directory matching a glob pattern.",
            json!({
                "pattern": {"type": "string", "maxLength": MAX_GLOB_BYTES, "description": "The glob pattern to match files against"},
                "path": {"type": "string", "description": "The directory to search in. If omitted, uses the working directory."}
            }),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if input.get("path").is_some_and(Value::is_null) {
            return Err(ToolError::Input("path must not be null".to_string()));
        }
        let input: GlobInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        validate_glob_pattern(&input.pattern)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let search = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        assert_external_search_target(ctx, &search).await?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let is_file = tokio::fs::metadata(&search)
            .await
            .is_ok_and(|meta| meta.is_file());
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if is_file {
            return Err(ToolError::Input(format!(
                "glob path must be a directory: {}",
                display_path(&search)
            )));
        }
        let cancel = ctx.cancel.clone();
        let root = search.clone();
        let deadline = Instant::now() + SEARCH_BUDGET;
        let worker = tokio::task::spawn_blocking(move || {
            let mut bounded = BTreeSet::new();
            let mut total = 0usize;
            let complete = walk(&root, &cancel, deadline, |path| {
                let relative = path.strip_prefix(&root).unwrap_or(path.as_path());
                let relative = relative.to_string_lossy().replace('\\', "/");
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default();
                if grep::wildcard_match(&input.pattern, &relative)
                    || grep::wildcard_match(&input.pattern, &name)
                {
                    total = total.saturating_add(1);
                    bounded.insert(path);
                    if bounded.len() > SEARCH_LIMIT {
                        bounded.pop_last();
                    }
                }
            })?;
            Ok::<_, ToolError>((bounded, total, complete))
        });
        let (bounded, total, complete) = tokio::select! {
            result = worker => result.map_err(|error| ToolError::Other(format!("glob worker failed: {error}")))??,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let rows = bounded.into_iter().collect::<Vec<_>>();
        let truncated = !complete || total > SEARCH_LIMIT;
        let output_rows = rows
            .iter()
            .map(|path| display_path(path))
            .collect::<Vec<_>>();
        let mut output = if output_rows.is_empty() {
            if complete {
                "No files found"
            } else {
                "No files found in the part searched"
            }
            .to_string()
        } else {
            output_rows.join("\n")
        };
        if total > SEARCH_LIMIT {
            output.push_str(
                "\n\n(Results are truncated: showing first 100 results. Consider using a more specific path or pattern.)",
            );
        }
        if !complete {
            output.push_str("\n\n(Search stopped after 30 seconds before every directory was visited, so results are incomplete. Use a more specific path or pattern.)");
        }
        let legacy_paths = rows
            .iter()
            .map(|path| {
                path.strip_prefix(&ctx.workdir)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        let mut result = json!({
            "title": relative_title(&search, &ctx.workdir),
            "metadata": {
                "count": output_rows.len(),
                "truncated": truncated,
            },
            "output": output,
            "paths": legacy_paths,
            "total": total,
        });
        if !complete {
            result["metadata"]["incomplete"] = json!(true);
        }
        Ok(result)
    }
}

#[derive(Deserialize)]
struct LsInput {
    path: Option<String>,
}

/// Resolve the `ls` target directory relative to the session workdir.
///
/// Leniency: an empty string `path` is treated the same as an omitted one
/// (the working directory) — models sometimes send `""` instead of leaving
/// the optional field out, and there is no directory an empty path could
/// unambiguously mean otherwise.
fn resolve_ls_dir(workdir: &Path, path: Option<String>) -> PathBuf {
    let raw_path = path.filter(|path| !path.is_empty());
    raw_path
        .as_deref()
        .map_or_else(|| workdir.to_path_buf(), |path| resolve_file(workdir, path))
}

/// Lists immediate directory entries (name, type, size) without recursion.
pub struct LsTool;
#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "ls",
            "List the immediate entries of a directory (name, type, size).",
            json!({"path": {"type": "string", "description": "Directory to list, relative to the session workdir unless absolute. Omit, or pass an empty string, to use the working directory."}}),
            &[],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: LsInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let dir = resolve_ls_dir(&ctx.workdir, input.path);
        assert_external_directory(ctx, &dir, true).await?;
        ctx.permission
            .assert(
                Action::Read,
                Resource::Path(dir.to_string_lossy().into_owned()),
            )
            .await?;
        let mut rows: Vec<(String, &'static str, u64)> = Vec::new();
        let mut rd = tokio::fs::read_dir(&dir).await.map_err(|error| {
            ToolError::Other(format!("ls failed for {}: {error}", display_path(&dir)))
        })?;
        while let Some(entry) = rd.next_entry().await? {
            let meta = entry.metadata().await?;
            let kind = if meta.is_dir() {
                "dir"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            rows.push((
                entry.file_name().to_string_lossy().into_owned(),
                kind,
                meta.len(),
            ));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let entries: Vec<Value> = rows
            .into_iter()
            .map(|(name, kind, size)| json!({ "name": name, "type": kind, "size": size }))
            .collect();
        Ok(json!({ "entries": entries }))
    }
}

#[derive(Deserialize)]
struct FindInput {
    pattern: String,
    path: Option<String>,
}
/// Compatibility path finder: recursive `*` matching with sizes and no result-row cap.
pub struct FindTool;
#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn schema(&self) -> ToolSchema {
        obj_schema(
            "find",
            "Recursively find files whose relative path or name matches a `*` glob, with size metadata.",
            json!({"pattern": {"type": "string"}, "path": {"type": "string"}}),
            &["pattern"],
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: FindInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let root = input.path.as_deref().map_or_else(
            || ctx.workdir.clone(),
            |path| resolve_file(&ctx.workdir, path),
        );
        ctx.permission
            .assert(Action::Glob, Resource::Glob(input.pattern.clone()))
            .await?;
        assert_external_directory(ctx, &root, true).await?;
        let cancel = ctx.cancel.clone();
        let search = root.clone();
        let deadline = Instant::now() + SEARCH_BUDGET;
        let worker = tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            let complete = walk(&search, &cancel, deadline, |path| files.push(path))?;
            Ok::<_, ToolError>((files, complete))
        });
        let (files, complete) = tokio::select! {
            result = worker => result.map_err(|error| ToolError::Other(format!("find worker failed: {error}")))??,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
        };
        let mut rows: Vec<(String, u64)> = Vec::new();
        for f in &files {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let rel = f.strip_prefix(&root).unwrap_or(f.as_path());
            let rel_str = rel.to_string_lossy();
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if glob_match(&input.pattern, &rel_str) || glob_match(&input.pattern, &name) {
                let size = tokio::fs::metadata(f).await.map(|m| m.len()).unwrap_or(0);
                rows.push((f.to_string_lossy().into_owned(), size));
            }
        }
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let results: Vec<Value> = rows
            .into_iter()
            .map(|(path, size)| json!({ "path": path, "size": size }))
            .collect();
        let mut result = json!({ "results": results });
        if !complete {
            result["incomplete"] = json!(true);
        }
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn walk_fixture() -> Result<PathBuf, std::io::Error> {
        let root = std::env::temp_dir().join(format!(
            "hya-walk-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(root.join("aaa/bbb/ccc"))?;
        std::fs::write(root.join("aaa/bbb/ccc/aaa"), "deep")?;
        std::fs::write(root.join("zzz"), "shallow")?;
        Ok(root)
    }

    #[test]
    fn expired_walk_is_incomplete() -> Result<(), Box<dyn std::error::Error>> {
        let root = walk_fixture()?;
        let mut files = Vec::new();
        assert!(!walk(
            &root,
            &CancellationToken::new(),
            Instant::now(),
            |path| files.push(path)
        )?);
        assert!(files.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn future_walk_visits_every_file_breadth_first() -> Result<(), Box<dyn std::error::Error>> {
        let root = walk_fixture()?;
        let mut files = Vec::new();
        assert!(walk(
            &root,
            &CancellationToken::new(),
            Instant::now() + SEARCH_BUDGET,
            |path| files.push(path)
        )?);
        assert_eq!(files, vec![root.join("zzz"), root.join("aaa/bbb/ccc/aaa")]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn ls_empty_path_resolves_to_workdir_same_as_omitted() {
        let workdir = PathBuf::from("/work/dir");
        assert_eq!(
            resolve_ls_dir(&workdir, Some(String::new())),
            resolve_ls_dir(&workdir, None)
        );
        assert_eq!(resolve_ls_dir(&workdir, None), workdir);
    }

    #[test]
    fn ls_relative_path_resolves_against_workdir_not_process_cwd() {
        let workdir = PathBuf::from("/work/dir");
        let resolved = resolve_ls_dir(&workdir, Some("sub".to_string()));
        assert_eq!(resolved, PathBuf::from("/work/dir/sub"));
    }

    #[test]
    fn ls_absolute_path_is_used_verbatim() {
        let workdir = PathBuf::from("/work/dir");
        let resolved = resolve_ls_dir(&workdir, Some("/elsewhere".to_string()));
        assert_eq!(resolved, PathBuf::from("/elsewhere"));
    }
}
