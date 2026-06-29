use std::error::Error;
use std::path::PathBuf;

use hya_sdk::ServerHandle;

use crate::args::Args;

pub(crate) enum ServerMode {
    Spawned(ServerHandle),
    Attached(String),
}

impl ServerMode {
    pub(crate) async fn new(
        args: &Args,
        directory: &str,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if let Some(base_url) = args.server.clone() {
            return Ok(Self::Attached(base_url));
        }
        if args.opencode {
            return Ok(Self::Spawned(ServerHandle::spawn(directory).await?));
        }
        let backend_bin = resolve_backend_bin(args);
        Ok(Self::Spawned(
            ServerHandle::spawn_hya_backend(&backend_bin, directory).await?,
        ))
    }

    pub(crate) fn base_url(&self) -> &str {
        match self {
            Self::Spawned(handle) => handle.base_url(),
            Self::Attached(base_url) => base_url,
        }
    }
}

/// Locate the vendored backend package that hosts the native bridge script. Overridable with
/// `HYA_BACKEND_DIR`; otherwise resolved relative to this binary's source tree.
pub(crate) fn resolve_backend_dir() -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    if let Ok(dir) = std::env::var("HYA_BACKEND_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../opencode-origin/packages/opencode");
    candidate.canonicalize().map_err(|e| {
        format!(
            "cannot locate backend package at {} ({e}); set HYA_BACKEND_DIR",
            candidate.display()
        )
        .into()
    })
}

/// Resolve the `hya-backend` binary to spawn. Order: `--backend-bin`, `HYA_BACKEND_BIN`, the sibling `release`
/// build, a `hya-backend` on `PATH`, then the sibling `debug` build. Release is preferred over `debug`
/// because the unoptimized debug binary is ~10x larger and far slower to cold-load (the cause of
/// slow backend starts); developers wanting a fresh debug backend can set `HYA_BACKEND_BIN`.
fn resolve_backend_bin(args: &Args) -> String {
    if let Some(bin) = &args.backend_bin {
        return bin.clone();
    }
    if let Ok(bin) = std::env::var("HYA_BACKEND_BIN") {
        return bin;
    }
    let sibling = |profile: &str| {
        workspace_target_bin(profile, "hya-backend")
            .canonicalize()
            .ok()
            .map(|path| path.display().to_string())
    };
    sibling("release")
        .or_else(backend_on_path)
        .or_else(|| sibling("debug"))
        .unwrap_or_else(|| "hya-backend".to_string())
}

fn workspace_target_bin(profile: &str, bin: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(profile)
        .join(bin)
}

/// First `hya-backend` executable found on `PATH`, if any.
fn backend_on_path() -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join("hya-backend");
        candidate.is_file().then(|| candidate.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_target_bin_points_at_current_workspace_target() {
        let path = workspace_target_bin("debug", "hya-backend");

        assert_eq!(
            path,
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target")
                .join("debug")
                .join("hya-backend")
        );
    }
}
