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
    if out.as_os_str().is_empty() {
        // A relative path that normalizes away entirely (e.g. joining "."
        // onto a "." workdir) still names a real directory: the current one.
        // An empty PathBuf is not a valid filesystem argument on any
        // platform, so collapse back to "." instead of silently failing
        // every syscall a caller makes against it.
        out.push(".");
    }
    out
}

#[must_use]
pub(crate) fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_to_current_dir_instead_of_empty() {
        // Regression: workdir "." + path "." used to normalize to an empty
        // PathBuf, which every fs syscall (read_dir, metadata, ...) rejects
        // with a content-free "No such file or directory".
        assert_eq!(normalize(Path::new("./.")), PathBuf::from("."));
        assert_eq!(normalize(Path::new(".")), PathBuf::from("."));
        assert_eq!(normalize(Path::new("a/..")), PathBuf::from("."));
    }

    #[test]
    fn resolve_file_with_dot_workdir_and_dot_path_stays_a_valid_directory() {
        let resolved = resolve_file(Path::new("."), ".");
        assert_eq!(resolved, PathBuf::from("."));
        assert!(!resolved.as_os_str().is_empty());
    }

    #[test]
    fn normalize_still_resolves_ordinary_relative_components() {
        assert_eq!(normalize(Path::new("a/./b/../c")), PathBuf::from("a/c"));
    }
}
