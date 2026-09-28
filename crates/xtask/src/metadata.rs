//! The workspace's own packages and targets, from `cargo metadata --no-deps`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

/// A workspace package.
#[derive(Debug, Deserialize)]
pub struct Package {
    /// Package name as written in its manifest.
    pub name: String,
    /// Path to the package's `Cargo.toml`.
    pub manifest_path: PathBuf,
    /// Every target the package declares or Cargo discovers.
    pub targets: Vec<Target>,
}

/// One build target of a [`Package`].
#[derive(Debug, Deserialize)]
pub struct Target {
    /// Target name as written in the manifest (may contain `-`).
    pub name: String,
    /// Target kinds (`lib`, `bin`, `test`, `cdylib`, …).
    pub kind: Vec<String>,
    /// Root source file.
    pub src_path: PathBuf,
}

impl Target {
    /// The name rustc uses for this target's crate and output files.
    pub fn crate_name(&self) -> String {
        self.name.replace('-', "_")
    }
}

#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

/// Run `cargo metadata --no-deps` in `root` and return the workspace packages.
///
/// # Errors
/// Returns an error when cargo cannot run or its output does not parse.
pub fn workspace_packages(root: &Path) -> Result<Vec<Package>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .context("run cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).context("parse cargo metadata output")?;
    Ok(metadata.packages)
}
