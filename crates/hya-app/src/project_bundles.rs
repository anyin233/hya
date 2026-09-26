//! Project bundle sources: `.hya/bundles/<name>/` source directories.
//!
//! Project bundles are the highest-precedence catalog tier of a registered
//! Project: they load from `<root>/.hya/bundles` of every Project root in
//! order (first root wins on id or namespace), a project bundle shadows an
//! installed bundle with the same identity id **or** the same namespace, and
//! content changes are republished at that Project's next bind via a content
//! fingerprint (see [`crate::ProjectScopeRefresh`]).
//!
//! [`install_project_bundle`] and [`remove_project_bundle`] manage this tier
//! for `hya bundle install|remove --project`. They follow the user registry's
//! rules: same-content reinstalls are unchanged, downgrades and namespace
//! takeovers need [`NamespaceInstallPolicy::OverwriteConflicts`], and an
//! install writes into a staging directory first and renames it into place.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use hya_bundle::{
    BundleCatalog, BundleError, BundleSource, PreparedCatalog, PreparedInstallableBundle,
    SourceFile,
};
use hya_store::{BundleInstallAction, NamespaceInstallPolicy, StoreError};

/// Project bundle directory of the current working directory:
/// `$CWD/.hya/bundles`. Only the local `hya bundle install|remove --project`
/// commands use it; the runtime loads project bundles per registered Project
/// from every Project root (see [`crate::ProjectScopeRefresh`]).
#[must_use]
pub fn project_bundles_dir() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join(".hya/bundles"))
}

/// One loaded project bundle: its prepared catalog, source directory, and the
/// content digest that feeds the directory fingerprint.
struct LoadedProjectBundle {
    prepared: PreparedCatalog,
    dir: PathBuf,
    content_digest: u64,
}

/// Load every project bundle under `dir` (one immediate subdirectory per
/// bundle, containing `bundle.yaml` or `bundle.hya.md`).
///
/// Unreadable or invalid bundles are skipped with a `tracing::warn!` so one
/// broken directory never wedges the whole catalog. Returns the prepared
/// catalogs sorted by bundle id plus a fingerprint that changes whenever any
/// project bundle file's contents change. Each bundle directory's own
/// `config.yml` (see [`crate::bundle_config`]) is not bundle content: it never
/// enters the prepared sources or this fingerprint.
#[must_use]
pub fn load_project_bundles(dir: &Path) -> (Vec<PreparedCatalog>, u64) {
    let (loaded, fingerprint) = load_project_bundle_dirs(dir);
    (
        loaded.into_iter().map(|(prepared, _)| prepared).collect(),
        fingerprint,
    )
}

/// [`load_project_bundles`] that also returns each bundle's source directory.
pub(crate) fn load_project_bundle_dirs(dir: &Path) -> (Vec<(PreparedCatalog, PathBuf)>, u64) {
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
                        dir: bundle_dir,
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
        catalogs.push((bundle.prepared, bundle.dir));
    }
    let mut fingerprint = fingerprint.finish();
    if fingerprint == 0 {
        fingerprint = 1;
    }
    (catalogs, fingerprint)
}

/// The project bundle directory of one Project root: `<root>/.hya/bundles`.
#[must_use]
pub fn root_bundles_dir(root: &Path) -> PathBuf {
    root.join(".hya/bundles")
}

/// Load the project bundles of every Project root, in root order.
///
/// Each root contributes `<root>/.hya/bundles` (see [`load_project_bundles`]).
/// The first root wins: a later bundle whose identity id **or** namespace an
/// earlier kept bundle already claims is skipped with a `tracing::warn!`, so
/// the result never carries two bundles with one id or one namespace. The
/// fingerprint changes whenever any root's bundle content or the root list
/// changes.
#[must_use]
pub fn load_project_bundles_for_roots(roots: &[PathBuf]) -> (Vec<PreparedCatalog>, u64) {
    let (loaded, fingerprint) = load_project_bundle_dirs_for_roots(roots);
    (
        loaded.into_iter().map(|(prepared, _)| prepared).collect(),
        fingerprint,
    )
}

/// [`load_project_bundles_for_roots`] that also returns each bundle's source
/// directory.
pub(crate) fn load_project_bundle_dirs_for_roots(
    roots: &[PathBuf],
) -> (Vec<(PreparedCatalog, PathBuf)>, u64) {
    let mut kept: Vec<(PreparedCatalog, PathBuf)> = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut namespaces = std::collections::BTreeSet::new();
    let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
    for root in roots {
        let (loaded, root_fingerprint) = load_project_bundle_dirs(&root_bundles_dir(root));
        root.hash(&mut fingerprint);
        root_fingerprint.hash(&mut fingerprint);
        for (prepared, dir) in loaded {
            let [bundle] = prepared.bundles() else {
                continue;
            };
            let id = bundle.identity().id.clone();
            let namespace = bundle.namespace().to_string();
            if ids.contains(&id) || namespaces.contains(&namespace) {
                tracing::warn!(
                    bundle_id = %id,
                    namespace = %namespace,
                    dir = %dir.display(),
                    "an earlier Project root already provides this project bundle id or namespace; skipped"
                );
                continue;
            }
            ids.insert(id);
            namespaces.insert(namespace);
            kept.push((prepared, dir));
        }
    }
    let mut fingerprint = fingerprint.finish();
    if fingerprint == 0 {
        fingerprint = 1;
    }
    (kept, fingerprint)
}

/// Cheap change detector for the project bundles of `roots`: the root list,
/// every bundle directory's content digest, and every bundle `config.yml`
/// digest (configuration changes models and restarts spawning bundles).
/// Nothing is prepared; equal digests mean [`load_project_bundle_dirs_for_roots`]
/// and the bundles' configuration would load the same.
///
/// Folds everything into `hasher`; returns whether any root has a bundle
/// directory.
pub(crate) fn project_roots_digest(roots: &[PathBuf], hasher: &mut sha2::Sha256) -> bool {
    use sha2::Digest as _;
    let mut any = false;
    for root in roots {
        hasher.update(b"root\0");
        hasher.update(root.as_os_str().as_encoded_bytes());
        let Ok(entries) = std::fs::read_dir(root_bundles_dir(root)) else {
            continue;
        };
        let mut dirs = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        dirs.sort();
        for dir in dirs {
            any = true;
            hasher.update(b"bundle\0");
            hasher.update(dir.as_os_str().as_encoded_bytes());
            hasher.update(directory_digest(&dir).to_le_bytes());
            match std::fs::read(dir.join(crate::bundle_config::BUNDLE_CONFIG_FILE_NAME)) {
                Ok(bytes) => {
                    hasher.update(b"config\0");
                    hasher.update(sha2::Sha256::digest(bytes));
                }
                Err(_) => hasher.update(b"no-config\0"),
            }
        }
    }
    any
}

/// Build a [`BundleSource`] from a directory: every regular file below it,
/// keyed by its path relative to the directory (forward slashes), except the
/// user-owned `config.yml` and its lock/temporary siblings. Returns `None`
/// when the directory carries no manifest at its root.
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
            if crate::bundle_config::is_bundle_config_entry(&keyed) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                files.push((keyed, bytes));
            }
        }
    }
}

/// Stable digest of every bundle file below `dir` (sorted relative path +
/// bytes); the bundle's `config.yml` is excluded like in [`directory_source`].
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

/// Failure managing a project-scope bundle.
#[derive(Debug, thiserror::Error)]
pub enum ProjectBundleError {
    /// A policy failure shared with the user registry (namespace conflict,
    /// downgrade, content conflict, reserved Agent id, or bundle not found).
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The bundle sources do not prepare.
    #[error(transparent)]
    Bundle(#[from] BundleError),
    /// The install target exists but is not a project bundle with this id.
    #[error(
        "PROJECT_BUNDLE_DIRECTORY_OCCUPIED: {path} exists and is not a project bundle for {bundle_id}; move or remove it first"
    )]
    DirectoryOccupied {
        /// Occupied directory.
        path: PathBuf,
        /// Bundle id being installed.
        bundle_id: String,
    },
    /// A filesystem operation failed.
    #[error("project bundle {path}: {detail}")]
    Io {
        /// Path the operation touched.
        path: PathBuf,
        /// Underlying error.
        detail: String,
    },
}

fn io_error(path: &Path, error: &std::io::Error) -> ProjectBundleError {
    ProjectBundleError::Io {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

/// One valid project bundle source directory.
#[derive(Clone, Debug)]
pub struct ProjectBundle {
    dir: PathBuf,
    prepared: std::sync::Arc<PreparedCatalog>,
    bundle: PreparedInstallableBundle,
}

impl ProjectBundle {
    /// Source directory under `.hya/bundles`.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Prepared single-bundle catalog.
    #[must_use]
    pub fn prepared(&self) -> &PreparedCatalog {
        &self.prepared
    }

    /// The prepared bundle.
    #[must_use]
    pub fn bundle(&self) -> &PreparedInstallableBundle {
        &self.bundle
    }

    /// Bundle identity id.
    #[must_use]
    pub fn bundle_id(&self) -> &str {
        &self.bundle.identity().id
    }

    /// Bundle identity version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.bundle.identity().version
    }
}

/// Prepare one source set into a [`ProjectBundle`] rooted at `dir`.
fn prepare_project_bundle(
    dir: PathBuf,
    source: BundleSource,
) -> Result<ProjectBundle, ProjectBundleError> {
    let prepared = hya_bundle::prepare_package(source)?;
    let [bundle] = prepared.bundles() else {
        return Err(StoreError::BundleRegistryData(
            "a project bundle must prepare to exactly one bundle".to_string(),
        )
        .into());
    };
    let bundle = bundle.clone();
    Ok(ProjectBundle {
        dir,
        prepared: std::sync::Arc::new(prepared),
        bundle,
    })
}

/// Every valid project bundle under `dir`, sorted by bundle id then
/// directory. Directories without a manifest or that fail to prepare are
/// skipped, as [`load_project_bundles`] skips them at runtime.
#[must_use]
pub fn project_bundles(dir: &Path) -> Vec<ProjectBundle> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut bundles = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let source = directory_source(&path)?;
            prepare_project_bundle(path, source).ok()
        })
        .collect::<Vec<_>>();
    bundles.sort_by(|left, right| {
        (left.bundle_id(), left.dir()).cmp(&(right.bundle_id(), right.dir()))
    });
    bundles
}

/// Directory name a fresh install uses for `bundle_id` (`acme/tools` ->
/// `acme__tools`). Bundle ids never contain `__` ambiguity that matters here:
/// an occupied name is refused rather than merged.
#[must_use]
pub fn project_bundle_dir_name(bundle_id: &str) -> String {
    bundle_id.replace('/', "__")
}

/// What [`install_project_bundle`] does (or did) with one bundle.
#[derive(Clone, Debug)]
pub struct ProjectInstallPlan {
    /// The incoming bundle.
    pub incoming: ProjectBundle,
    /// Directory the bundle is (or will be) written to.
    pub target: PathBuf,
    /// Effect on the incoming bundle id.
    pub action: BundleInstallAction,
    /// Project bundles removed because the incoming bundle takes over their
    /// namespace.
    pub displaced: Vec<ProjectBundle>,
}

/// Reject any source path that could escape the bundle directory.
fn validate_source_paths(files: &[SourceFile]) -> Result<(), ProjectBundleError> {
    for file in files {
        let path = file.path();
        let unsafe_path = path.is_empty()
            || path.starts_with('/')
            || path.contains('\\')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..");
        if unsafe_path {
            return Err(BundleError::InvalidSourcePath {
                source_name: "project-install".to_string(),
                path: path.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

/// Drop the user-owned `config.yml` (and its lock/temporary siblings) from
/// incoming sources: it is configuration, never bundle content, so an
/// install neither ships nor overwrites it.
fn without_config_entries(files: &[SourceFile]) -> Vec<SourceFile> {
    files
        .iter()
        .filter(|file| !crate::bundle_config::is_bundle_config_entry(file.path()))
        .cloned()
        .collect()
}

/// Decide a project install of `files` into `dir` without writing anything.
///
/// A root-level `config.yml` in `files` is ignored (it is the bundle's
/// user-owned configuration file, not bundle content).
///
/// # Errors
/// Returns the same policy errors the user registry reports
/// ([`StoreError::NamespaceConflict`], [`StoreError::BundleDowngradeRequired`],
/// [`StoreError::BundleContentConflict`], [`StoreError::BundleAgentIdReserved`]),
/// a prepare failure, or [`ProjectBundleError::DirectoryOccupied`]. Under
/// [`NamespaceInstallPolicy::OverwriteConflicts`] a same-version content
/// change replaces the directory instead of failing.
pub fn plan_project_install(
    dir: &Path,
    files: &[SourceFile],
    reserved_agent_ids: &[&str],
    policy: NamespaceInstallPolicy,
) -> Result<ProjectInstallPlan, ProjectBundleError> {
    validate_source_paths(files)?;
    let mut incoming = prepare_project_bundle(
        PathBuf::new(),
        BundleSource::new("project-install", without_config_entries(files)),
    )?;
    let bundle_id = incoming.bundle_id().to_string();
    let version = incoming.version().to_string();
    for agent in incoming.bundle().agents() {
        if reserved_agent_ids.contains(&agent.id.as_str()) {
            return Err(StoreError::BundleAgentIdReserved {
                bundle_id,
                agent_id: agent.id.as_str().to_string(),
            }
            .into());
        }
    }

    let installed = project_bundles(dir);
    let existing = installed
        .iter()
        .find(|bundle| bundle.bundle_id() == bundle_id);
    let same_content =
        existing.is_some_and(|bundle| bundle.prepared().digest() == incoming.prepared().digest());
    if let Some(bundle) = existing
        && !same_content
        && bundle.version() != version
        && hya_store::is_downgrade(&version, bundle.version())
        && policy == NamespaceInstallPolicy::DenyConflicts
    {
        return Err(StoreError::BundleDowngradeRequired {
            bundle_id,
            installed_version: bundle.version().to_string(),
            incoming_version: version,
        }
        .into());
    }

    let namespace = incoming.bundle().namespace().to_string();
    let displaced = installed
        .iter()
        .filter(|bundle| {
            bundle.bundle_id() != bundle_id && bundle.bundle().namespace() == namespace
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(owner) = displaced.first()
        && policy == NamespaceInstallPolicy::DenyConflicts
    {
        return Err(StoreError::NamespaceConflict {
            namespace,
            existing_bundle_id: owner.bundle_id().to_string(),
            incoming_bundle_id: bundle_id,
        }
        .into());
    }

    let action = match existing {
        None => BundleInstallAction::Install,
        Some(_) if same_content => BundleInstallAction::Unchanged,
        Some(bundle)
            if bundle.version() == version && policy == NamespaceInstallPolicy::DenyConflicts =>
        {
            return Err(StoreError::BundleContentConflict { bundle_id, version }.into());
        }
        Some(bundle) => BundleInstallAction::Replace {
            installed_version: bundle.version().to_string(),
        },
    };

    let target = match existing {
        Some(bundle) => bundle.dir().to_path_buf(),
        None => {
            let target = dir.join(project_bundle_dir_name(&bundle_id));
            if std::fs::symlink_metadata(&target).is_ok() {
                return Err(ProjectBundleError::DirectoryOccupied {
                    path: target,
                    bundle_id,
                });
            }
            target
        }
    };

    let mut complete = installed
        .iter()
        .filter(|bundle| {
            bundle.bundle_id() != bundle_id
                && !displaced
                    .iter()
                    .any(|loser| loser.bundle_id() == bundle.bundle_id())
        })
        .map(|bundle| bundle.bundle().clone())
        .collect::<Vec<_>>();
    complete.push(incoming.bundle().clone());
    BundleCatalog::from_prepared(&complete).map_err(StoreError::from)?;

    incoming.dir.clone_from(&target);
    Ok(ProjectInstallPlan {
        incoming,
        target,
        action,
        displaced,
    })
}

/// Unique sibling path under `parent` for staging or backups.
fn scratch_path(parent: &Path, label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    parent.join(format!(
        ".bundle-{label}-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn write_files(root: &Path, files: &[SourceFile]) -> Result<(), ProjectBundleError> {
    for file in files {
        let path = root.join(file.path());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(parent, &error))?;
        }
        std::fs::write(&path, file.bytes()).map_err(|error| io_error(&path, &error))?;
    }
    Ok(())
}

/// Install `files` as a project bundle under `dir` (usually `.hya/bundles`).
///
/// The sources are written to a staging directory beside `dir` and renamed
/// into place, so the runtime loader never sees a half-written bundle; a
/// replaced directory is restored if the final rename fails. Returns the
/// executed plan; an [`BundleInstallAction::Unchanged`] plan writes nothing.
/// A replaced bundle keeps its existing `config.yml`; incoming sources never
/// write one.
///
/// # Errors
/// Everything [`plan_project_install`] reports, plus filesystem failures.
pub fn install_project_bundle(
    dir: &Path,
    files: Vec<SourceFile>,
    reserved_agent_ids: &[&str],
    policy: NamespaceInstallPolicy,
) -> Result<ProjectInstallPlan, ProjectBundleError> {
    let plan = plan_project_install(dir, &files, reserved_agent_ids, policy)?;
    if plan.action == BundleInstallAction::Unchanged {
        return Ok(plan);
    }
    let files = without_config_entries(&files);
    std::fs::create_dir_all(dir).map_err(|error| io_error(dir, &error))?;
    let scratch_parent = dir.parent().unwrap_or(dir);
    let staging = scratch_path(scratch_parent, "staging");
    let staged = write_files(&staging, &files).and_then(|()| {
        let config = plan
            .target
            .join(crate::bundle_config::BUNDLE_CONFIG_FILE_NAME);
        if config.is_file() {
            let preserved = staging.join(crate::bundle_config::BUNDLE_CONFIG_FILE_NAME);
            std::fs::copy(&config, &preserved).map_err(|error| io_error(&config, &error))?;
        }
        Ok(())
    });
    if let Err(error) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    let backup = plan
        .target
        .exists()
        .then(|| scratch_path(scratch_parent, "replaced"));
    if let Some(backup) = &backup
        && let Err(error) = std::fs::rename(&plan.target, backup)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(io_error(&plan.target, &error));
    }
    if let Err(error) = std::fs::rename(&staging, &plan.target) {
        if let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &plan.target);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(io_error(&plan.target, &error));
    }
    if let Some(backup) = &backup {
        std::fs::remove_dir_all(backup).map_err(|error| io_error(backup, &error))?;
    }
    for loser in &plan.displaced {
        std::fs::remove_dir_all(loser.dir()).map_err(|error| io_error(loser.dir(), &error))?;
    }
    Ok(plan)
}

/// Find the project bundle with `bundle_id` under `dir` without removing it.
///
/// # Errors
/// [`StoreError::BundleNotFound`] when no valid project bundle has that id.
pub fn find_project_bundle(
    dir: &Path,
    bundle_id: &str,
) -> Result<ProjectBundle, ProjectBundleError> {
    project_bundles(dir)
        .into_iter()
        .find(|bundle| bundle.bundle_id() == bundle_id)
        .ok_or_else(|| {
            StoreError::BundleNotFound {
                bundle_id: bundle_id.to_string(),
            }
            .into()
        })
}

/// Delete the source directory of the project bundle with `bundle_id`.
///
/// # Errors
/// [`StoreError::BundleNotFound`] when no valid project bundle has that id, or
/// a filesystem failure.
pub fn remove_project_bundle(
    dir: &Path,
    bundle_id: &str,
) -> Result<ProjectBundle, ProjectBundleError> {
    let bundle = find_project_bundle(dir, bundle_id)?;
    std::fs::remove_dir_all(bundle.dir()).map_err(|error| io_error(bundle.dir(), &error))?;
    Ok(bundle)
}
