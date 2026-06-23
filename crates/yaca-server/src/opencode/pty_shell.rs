use std::collections::BTreeSet;
use std::path::Path;

use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct ShellItem {
    path: String,
    name: String,
    acceptable: bool,
}

pub(super) async fn shells() -> Json<Vec<ShellItem>> {
    Json(shell_candidates().into_iter().map(shell_item).collect())
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

fn shell_item(path: String) -> ShellItem {
    let acceptable = is_executable(&path);
    let name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path.as_str())
        .to_string();
    ShellItem {
        path,
        name,
        acceptable,
    }
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
