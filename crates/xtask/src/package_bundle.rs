//! Deterministic public `.hyabundle` packaging.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use hya_bundle::{BundleSource, write_public_package};

/// Validate one source directory and atomically write canonical public package bytes.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let [source, output] = args.as_slice() else {
        bail!("usage: cargo xtask package-bundle <source-directory> <output.hyabundle>");
    };
    let output = PathBuf::from(output);
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create package output directory {}", parent.display()))?;

    let source = BundleSource::read_directory(source)
        .with_context(|| format!("read bundle source directory {source}"))?;
    let bytes = write_public_package(&source).context("write deterministic public bundle")?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bundle.hyabundle"),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)
        .with_context(|| format!("write temporary package {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &output) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("activate package output {}", output.display()));
    }
    Ok(())
}
