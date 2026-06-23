use std::path::{Component, Path, PathBuf};

#[must_use]
pub(crate) fn resolve_file(workdir: &Path, file_path: &str) -> PathBuf {
    let candidate = Path::new(file_path);
    if candidate.is_absolute() {
        normalize(candidate)
    } else {
        normalize(&workdir.join(candidate))
    }
}

#[must_use]
pub(crate) fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    }
}

#[must_use]
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[must_use]
pub(crate) fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[must_use]
pub(crate) fn file_uri(path: &Path) -> String {
    format!("file://{}", display_path(path))
}
