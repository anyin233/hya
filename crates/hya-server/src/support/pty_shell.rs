use std::collections::BTreeSet;

/// Acceptable shell binary paths for v1 shell listing.
pub(crate) fn shell_paths() -> Vec<std::path::PathBuf> {
    shell_candidates()
        .into_iter()
        .filter(|path| is_executable(path))
        .map(std::path::PathBuf::from)
        .collect()
}

fn shell_candidates() -> Vec<String> {
    let mut paths = BTreeSet::new();
    if let Some(shell) = std::env::var_os("SHELL").and_then(|value| value.into_string().ok()) {
        paths.insert(shell);
    }
    for path in [
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/sh",
        "/usr/bin/sh",
    ] {
        paths.insert(path.to_string());
    }
    paths.into_iter().collect()
}

fn is_executable(path: &str) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
