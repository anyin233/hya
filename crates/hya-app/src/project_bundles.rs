//! Project bundle sources: `.hya/bundles/<name>/` source directories.
//!
//! Project bundles are the highest-precedence catalog tier: a project bundle
//! shadows an installed bundle with the same identity id **or** the same
//! namespace, and content changes are republished at root bind boundaries via
//! a content fingerprint (the registry generation does not move for files).

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use hya_bundle::{BundleSource, PreparedCatalog, SourceFile};

/// Default project bundle directory: `$CWD/.hya/bundles` (the backend process
/// working directory at startup, mirroring [`crate::plugins::plugins_dir`]).
#[must_use]
pub fn project_bundles_dir() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".hya/bundles"))
}

/// One loaded project bundle: its prepared catalog plus the content digest
/// that feeds the directory fingerprint.
struct LoadedProjectBundle {
    prepared: PreparedCatalog,
    content_digest: u64,
}

/// Load every project bundle under `dir` (one immediate subdirectory per
/// bundle, containing `bundle.yaml` or `bundle.hya.md`).
///
/// Unreadable or invalid bundles are skipped with a `tracing::warn!` so one
/// broken directory never wedges the whole catalog. Returns the prepared
/// catalogs sorted by bundle id plus a fingerprint that changes whenever any
/// project bundle file's contents change.
#[must_use]
pub fn load_project_bundles(dir: &Path) -> (Vec<PreparedCatalog>, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), 0);
    };
    let mut loaded: BTreeMap<String, LoadedProjectBundle> = BTreeMap::new();
    for entry in entries.flatten() {
        let bundle_dir = entry.path();
        if !bundle_dir.is_dir() {
            continue;
        }
        let Some(source) = directory_source(&bundle_dir) else {
            continue;
        };
        match hya_bundle::prepare_package(source) {
            Ok(prepared) => {
                let [bundle] = prepared.bundles() else {
                    tracing::warn!(
                        dir = %bundle_dir.display(),
                        "project bundle must prepare to exactly one bundle; skipped"
                    );
                    continue;
                };
                let identity = format!("{}@{}", bundle.identity().id, bundle.identity().version);
                let content_digest = directory_digest(&bundle_dir);
                loaded.insert(
                    identity,
                    LoadedProjectBundle {
                        prepared,
                        content_digest,
                    },
                );
            }
            Err(error) => {
                tracing::warn!(
                    dir = %bundle_dir.display(),
                    error = %error,
                    "skipping invalid project bundle"
                );
            }
        }
    }
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    let mut catalogs = Vec::with_capacity(loaded.len());
    for (identity, bundle) in &loaded {
        identity.hash(&mut fingerprint);
        bundle.content_digest.hash(&mut fingerprint);
    }
    for bundle in loaded.into_values() {
        catalogs.push(bundle.prepared);
    }
    let mut fingerprint = fingerprint.finish();
    if fingerprint == 0 {
        fingerprint = 1;
    }
    (catalogs, fingerprint)
}

/// Build a [`BundleSource`] from a directory: every regular file below it,
/// keyed by its path relative to the directory (forward slashes). Returns
/// `None` when the directory carries no manifest at its root.
fn directory_source(dir: &Path) -> Option<BundleSource> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files);
    let has_manifest = files
        .iter()
        .any(|(path, _)| path == "bundle.yaml" || path == "bundle.hya.md");
    if !has_manifest {
        return None;
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Some(BundleSource::new(
        dir.to_string_lossy().into_owned(),
        files
            .into_iter()
            .map(|(path, bytes)| SourceFile::new(path, bytes))
            .collect(),
    ))
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else if path.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            let keyed = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if let Ok(bytes) = std::fs::read(&path) {
                files.push((keyed, bytes));
            }
        }
    }
}

/// Stable digest of every file below `dir` (sorted relative path + bytes).
fn directory_digest(dir: &Path) -> u64 {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, bytes) in &files {
        path.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    hasher.finish()
}
