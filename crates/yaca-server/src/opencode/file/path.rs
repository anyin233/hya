use std::path::{Component, Path, PathBuf};

use crate::ApiError;

pub(super) fn resolve_existing(root: &Path, query_path: &str) -> Result<PathBuf, ApiError> {
    let path = join_under(root, query_path)?;
    let resolved = std::fs::canonicalize(path).map_err(|e| ApiError::internal(e.to_string()))?;
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(ApiError::bad_request("path escapes workdir"))
    }
}

pub(super) fn join_under(root: &Path, query_path: &str) -> Result<PathBuf, ApiError> {
    let input = Path::new(query_path);
    if input.is_absolute() {
        return Err(ApiError::bad_request("absolute paths are not supported"));
    }
    let mut out = root.to_path_buf();
    for component in input.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                out.pop();
                if !out.starts_with(root) {
                    return Err(ApiError::bad_request("path escapes workdir"));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ApiError::bad_request("absolute paths are not supported"));
            }
        }
    }
    Ok(out)
}

pub(super) fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map_or(path, |relative| relative)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(super) fn entry_kind(is_dir: bool, is_file: bool) -> Option<&'static str> {
    if is_dir {
        Some("directory")
    } else if is_file {
        Some("file")
    } else {
        None
    }
}

pub(super) fn matches_kind(path: &Path, kind: Option<&str>) -> bool {
    match kind {
        Some("file") => path.is_file(),
        Some("directory") => path.is_dir(),
        _ => path.is_file() || path.is_dir(),
    }
}

pub(super) fn collect_paths(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() {
            collect_paths(&path, out);
        }
    }
}
