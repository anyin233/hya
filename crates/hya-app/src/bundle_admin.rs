//! Bundle management shared by `hya bundle` and the server's bundle routes
//! (`ListBundles`, `InstallBundle`, `UninstallBundle`, `SetBundleEnabled`):
//! the user registry, `.hya/bundles` project directories, package
//! validation and self-check, and the disabled set. [`BundleManager`] is the
//! server's [`BundleControl`].

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use hya_bundle::{
    BundleCatalog, PackageInspection, PreparedCatalog, PreparedCheck, PreparedInstallableBundle,
    PublicPackageInspection, cleanup_orphaned_staging, stage_package,
};
use hya_server::{
    BUNDLE_CONFLICT, BUNDLE_CONTROL_FAILURE, BUNDLE_IMMUTABLE, BUNDLE_INVALID_REQUEST,
    BUNDLE_NOT_FOUND, BundleControl, BundleControlError, BundleControlFuture, BundleListing,
    BundleListingComponents,
};
use hya_store::{
    BundleInstallAction, BundleInstallCandidate, BundleInstallOutcome, BundleRegistry,
    BundleRegistryRecord, BundleUninstallOutcome, NamespaceInstallPolicy, StoreError,
};

use crate::project_bundles::{
    ProjectBundleError, find_project_bundle, install_project_bundle, project_bundles,
    remove_project_bundle,
};

/// `ListBundles` scope labels.
pub const SCOPE_USER: &str = "user";
/// Project bundles (`<directory>/.hya/bundles`).
pub const SCOPE_PROJECT: &str = "project";
/// Trusted presets and first-party bundles shipped with hya.
pub const SCOPE_FIRST_PARTY: &str = "first_party";

/// Lines kept from a failed self-check's output.
const CHECK_TAIL_LINES: usize = 40;

fn invalid(message: impl Into<String>) -> BundleControlError {
    BundleControlError::new(BUNDLE_INVALID_REQUEST, message)
}

fn failure(message: impl std::fmt::Display) -> BundleControlError {
    BundleControlError::new(BUNDLE_CONTROL_FAILURE, message.to_string())
}

/// A registry or project failure, with the policy failures `overwrite` resolves explained.
#[must_use]
pub fn store_error(error: StoreError) -> BundleControlError {
    match error {
        StoreError::NamespaceConflict {
            namespace,
            existing_bundle_id,
            incoming_bundle_id,
        } => BundleControlError::new(
            BUNDLE_CONFLICT,
            format!(
                "NAMESPACE_CONFLICT: namespace {namespace} is owned by {existing_bundle_id}; \
                 install with overwrite (`--overwrite`) to replace it with {incoming_bundle_id}"
            ),
        ),
        StoreError::BundleDowngradeRequired {
            bundle_id,
            installed_version,
            incoming_version,
        } => BundleControlError::new(
            BUNDLE_CONFLICT,
            format!(
                "BUNDLE_DOWNGRADE_REQUIRED: {bundle_id} is installed at {installed_version}; \
                 install with overwrite (`--overwrite`) to install {incoming_version}"
            ),
        ),
        StoreError::BundleContentConflict { bundle_id, version } => BundleControlError::new(
            BUNDLE_CONFLICT,
            format!(
                "BUNDLE_CONTENT_CONFLICT: project bundle {bundle_id} {version} has different \
                 content; bump the version or install with overwrite (`--overwrite`)"
            ),
        ),
        error @ StoreError::BundleNotFound { .. } => {
            BundleControlError::new(BUNDLE_NOT_FOUND, error.to_string())
        }
        error @ (StoreError::BundleAgentIdReserved { .. }
        | StoreError::PrivateActivationUnsupported) => invalid(error.to_string()),
        error => failure(error),
    }
}

/// A project bundle failure (see [`store_error`]).
#[must_use]
pub fn project_error(error: ProjectBundleError) -> BundleControlError {
    match error {
        ProjectBundleError::Store(error) => store_error(error),
        ProjectBundleError::Bundle(error) => invalid(error.to_string()),
        error @ ProjectBundleError::DirectoryOccupied { .. } => {
            BundleControlError::new(BUNDLE_CONFLICT, error.to_string())
        }
        error => failure(error),
    }
}

/// Open the user registry, creating it when missing.
///
/// # Errors
/// The registry directory cannot be created or the database opened.
pub async fn open_registry() -> Result<BundleRegistry, BundleControlError> {
    let path = crate::bundle_registry_path();
    let parent = path
        .parent()
        .ok_or_else(|| failure("bundle registry path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        failure(format!(
            "create bundle registry directory {}: {error}",
            parent.display()
        ))
    })?;
    connect(&path).await
}

/// Open the user registry only when it already exists, so reads never create it.
///
/// # Errors
/// The registry exists but cannot be opened.
pub async fn existing_registry() -> Result<Option<BundleRegistry>, BundleControlError> {
    let path = crate::bundle_registry_path();
    let exists = path.try_exists().map_err(|error| {
        failure(format!(
            "inspect bundle registry path {}: {error}",
            path.display()
        ))
    })?;
    if !exists {
        return Ok(None);
    }
    connect(&path).await.map(Some)
}

async fn connect(path: &Path) -> Result<BundleRegistry, BundleControlError> {
    let path = path
        .to_str()
        .ok_or_else(|| failure("bundle registry path is not valid UTF-8"))?;
    BundleRegistry::connect(path)
        .await
        .map_err(|error| failure(format!("open bundle registry: {error}")))
}

/// Require the exact lowercase `.hyabundle` suffix.
///
/// # Errors
/// The file name lacks it.
pub fn validate_package_path(package: &Path) -> Result<(), BundleControlError> {
    let ok = package
        .file_name()
        .and_then(|filename| filename.to_str())
        .is_some_and(|filename| filename.ends_with(".hyabundle"));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "{}: a bundle package needs the exact lowercase .hyabundle suffix",
            package.display()
        )))
    }
}

/// Stage and inspect a package (public or private) without installing it.
///
/// # Errors
/// The package cannot be read, staged, or decoded.
pub fn inspect_package(package: &Path) -> Result<PackageInspection, BundleControlError> {
    let registry_path = crate::bundle_registry_path();
    let staging_root = registry_path
        .parent()
        .ok_or_else(|| failure("bundle registry path has no parent"))?
        .join("staging");
    cleanup_orphaned_staging(&staging_root)
        .map_err(|error| failure(format!("clean bundle staging directory: {error}")))?;
    stage_package(package, &staging_root)
        .map_err(|error| {
            invalid(format!(
                "stage bundle package {}: {error}",
                package.display()
            ))
        })?
        .inspect()
        .map_err(|error| {
            invalid(format!(
                "inspect bundle package {}: {error}",
                package.display()
            ))
        })
}

/// Inspect a package for install: a public package with exactly one bundle
/// that neither overrides an immutable trusted preset nor breaks the
/// first-party catalog.
///
/// # Errors
/// Any of those checks fails.
pub fn inspect_installable(package: &Path) -> Result<PublicPackageInspection, BundleControlError> {
    validate_package_path(package)?;
    let public = match inspect_package(package)? {
        PackageInspection::Public(public) => public,
        PackageInspection::Private(_) => {
            return Err(store_error(StoreError::PrivateActivationUnsupported));
        }
    };
    let [incoming] = public.prepared.bundles() else {
        return Err(invalid("a public package must contain exactly one bundle"));
    };
    let presets = crate::trusted_preset_inventory().map_err(failure)?;
    if let Some(preset) = presets
        .iter()
        .find(|preset| preset.id == incoming.identity().id)
    {
        return Err(BundleControlError::new(
            BUNDLE_IMMUTABLE,
            format!(
                "immutable trusted preset `{}` cannot be installed or overridden",
                preset.id
            ),
        ));
    }
    let mut first_party = crate::first_party_catalogs().map_err(failure)?;
    first_party.retain(|catalog| {
        catalog.bundles().first().is_none_or(|bundle| {
            bundle.identity().id != incoming.identity().id
                && bundle.namespace() != incoming.namespace()
        })
    });
    let mut catalogs = first_party.iter().collect::<Vec<_>>();
    catalogs.push(&public.prepared);
    BundleCatalog::from_verified_catalogs(&catalogs).map_err(|error| {
        invalid(format!(
            "validate package against the immutable first-party catalog: {error}"
        ))
    })?;
    Ok(public)
}

/// What a package's declared self-check did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelfCheck {
    /// The manifest declares none.
    None,
    /// The command (as shown) exited successfully.
    Passed(String),
}

/// Run the bundle's declared self-check (`check.command`) in a private copy
/// of the package's source files, before anything is installed: cwd = that
/// copy, env `HYA_BUNDLE_ID`, `HYA_BUNDLE_VERSION`, `HYA_BUNDLE_ROOT`, stdin
/// closed, killed at `check.timeout_secs`.
///
/// # Errors
/// The check fails, times out, or cannot run.
pub async fn run_self_check(
    public: &PublicPackageInspection,
) -> Result<SelfCheck, BundleControlError> {
    let [bundle] = public.prepared.bundles() else {
        return Err(invalid("a public package must contain exactly one bundle"));
    };
    let Some(check) = bundle.check() else {
        return Ok(SelfCheck::None);
    };
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let root =
        std::env::temp_dir().join(format!("hya-bundle-check-{}-{nonce:x}", std::process::id()));
    let result = run_self_check_in(&root, public, bundle, check).await;
    let _ = fs::remove_dir_all(&root);
    result.map(SelfCheck::Passed)
}

async fn run_self_check_in(
    root: &Path,
    public: &PublicPackageInspection,
    bundle: &PreparedInstallableBundle,
    check: &PreparedCheck,
) -> Result<String, BundleControlError> {
    let write = |path: &Path, bytes: &[u8]| -> Result<(), BundleControlError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| failure(format!("create {}: {error}", parent.display())))?;
        }
        fs::write(path, bytes)
            .map_err(|error| failure(format!("write {}: {error}", path.display())))
    };
    for file in &public.files {
        write(&root.join(file.path()), file.bytes())?;
    }
    let (program, args) = check
        .command
        .split_first()
        .ok_or_else(|| invalid("check.command is empty"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(root)
        .env("HYA_BUNDLE_ID", &bundle.identity().id)
        .env("HYA_BUNDLE_VERSION", &bundle.identity().version)
        .env("HYA_BUNDLE_ROOT", root)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let shown = check.command.join(" ");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(check.timeout_secs),
        command.output(),
    )
    .await
    .map_err(|_| {
        invalid(format!(
            "self-check `{shown}` timed out after {} s; nothing was installed",
            check.timeout_secs
        ))
    })?
    .map_err(|error| invalid(format!("run self-check `{shown}`: {error}")))?;
    if !output.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(CHECK_TAIL_LINES)..].join("\n");
        return Err(invalid(format!(
            "self-check `{shown}` failed ({}); nothing was installed\n{tail}",
            output.status
        )));
    }
    Ok(shown)
}

/// Built-in agent ids an installed bundle must not claim.
#[must_use]
pub fn reserved_agent_ids() -> Vec<&'static str> {
    hya_core::builtin_agents()
        .iter()
        .map(|agent| agent.id)
        .collect()
}

/// The registry candidate of an inspected package.
#[must_use]
pub fn install_candidate(public: &PublicPackageInspection) -> BundleInstallCandidate {
    BundleInstallCandidate {
        source_digest: public.source_digest,
        prepared_digest: public.prepared.digest().to_owned(),
        prepared_bytes: public.prepared.bytes().to_vec(),
        installed_at: hya_proto::now_millis(),
    }
}

/// Where an install writes.
pub enum InstallTarget<'a> {
    /// The user registry.
    User(&'a BundleRegistry),
    /// A `.hya/bundles` directory.
    Project(&'a Path),
}

/// What an install did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Installed {
    /// What changed.
    pub action: BundleInstallAction,
    /// The registry generation (user installs).
    pub generation: Option<u64>,
    /// The project bundle directory (project installs).
    pub path: Option<PathBuf>,
}

/// Install an inspected, self-checked package.
///
/// # Errors
/// The install conflicts or the registry/project write fails.
pub async fn install(
    public: PublicPackageInspection,
    policy: NamespaceInstallPolicy,
    target: InstallTarget<'_>,
) -> Result<Installed, BundleControlError> {
    match target {
        InstallTarget::User(registry) => {
            let outcome = registry
                .install(&reserved_agent_ids(), policy, install_candidate(&public))
                .await
                .map_err(store_error)?;
            let (action, generation) = match outcome {
                BundleInstallOutcome::Installed { generation } => {
                    (BundleInstallAction::Install, generation)
                }
                BundleInstallOutcome::Replaced { generation } => (
                    BundleInstallAction::Replace {
                        installed_version: String::new(),
                    },
                    generation,
                ),
                BundleInstallOutcome::Unchanged { generation } => {
                    (BundleInstallAction::Unchanged, generation)
                }
            };
            Ok(Installed {
                action,
                generation: Some(generation),
                path: None,
            })
        }
        InstallTarget::Project(dir) => {
            let plan = install_project_bundle(dir, public.files, &reserved_agent_ids(), policy)
                .map_err(project_error)?;
            Ok(Installed {
                action: plan.action,
                generation: None,
                path: Some(plan.target),
            })
        }
    }
}

/// Refuse trusted presets and first-party bundles: they cannot be removed or disabled.
fn ensure_mutable(bundle_id: &str, verb: &str) -> Result<(), BundleControlError> {
    let presets = crate::trusted_preset_inventory().map_err(failure)?;
    if presets.iter().any(|preset| preset.id == bundle_id) {
        return Err(BundleControlError::new(
            BUNDLE_IMMUTABLE,
            format!("immutable trusted preset `{bundle_id}` cannot be {verb}"),
        ));
    }
    Ok(())
}

/// What a removal deleted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Removed {
    /// A registry row; the new registry generation.
    User {
        /// Registry generation after the removal.
        generation: u64,
    },
    /// A project bundle directory.
    Project {
        /// The deleted directory.
        path: PathBuf,
    },
}

/// Remove an installed bundle from the user registry, or from `project_dir`.
///
/// # Errors
/// The id is a preset or first-party bundle, is not installed there, or the removal fails.
pub async fn remove(
    bundle_id: &str,
    project_dir: Option<&Path>,
) -> Result<Removed, BundleControlError> {
    ensure_mutable(bundle_id, "removed")?;
    if let Some(dir) = project_dir {
        let removed = remove_project_bundle(dir, bundle_id).map_err(project_error)?;
        return Ok(Removed::Project {
            path: removed.dir().to_path_buf(),
        });
    }
    let installed = match existing_registry().await? {
        Some(registry) => {
            let found = registry
                .snapshot()
                .await
                .map_err(store_error)?
                .bundles
                .iter()
                .any(|record| record.bundle_id == bundle_id);
            found.then_some(registry)
        }
        None => None,
    };
    let Some(registry) = installed else {
        let first_party = crate::first_party_catalogs()
            .map_err(failure)?
            .iter()
            .any(|catalog| {
                catalog
                    .bundles()
                    .iter()
                    .any(|bundle| bundle.identity().id == bundle_id)
            });
        if first_party {
            return Err(BundleControlError::new(
                BUNDLE_IMMUTABLE,
                format!("immutable first-party bundle `{bundle_id}` cannot be removed"),
            ));
        }
        return Err(store_error(StoreError::BundleNotFound {
            bundle_id: bundle_id.to_string(),
        }));
    };
    let BundleUninstallOutcome::Removed { generation } =
        registry.uninstall(bundle_id).await.map_err(store_error)?;
    Ok(Removed::User { generation })
}

/// Look up a project bundle for a confirmation prompt.
///
/// # Errors
/// It is not installed in `dir`.
pub fn project_bundle(
    dir: &Path,
    bundle_id: &str,
) -> Result<crate::project_bundles::ProjectBundle, BundleControlError> {
    find_project_bundle(dir, bundle_id).map_err(project_error)
}

/// Enable or disable a bundle id in every scope.
///
/// # Errors
/// The id is a trusted preset (always on) or the registry write fails.
pub async fn set_enabled(bundle_id: &str, enabled: bool) -> Result<(), BundleControlError> {
    ensure_mutable(bundle_id, "disabled")?;
    open_registry()
        .await?
        .set_bundle_enabled(bundle_id, enabled)
        .await
        .map_err(store_error)?;
    Ok(())
}

/// Decode one registry row's prepared catalog, checking it matches the row.
///
/// # Errors
/// The row is corrupt or was written by an incompatible version.
pub fn decode_installed_catalog(
    record: &BundleRegistryRecord,
) -> Result<PreparedCatalog, StoreError> {
    let corrupt = || StoreError::BundleRegistryCorrupt {
        bundle_id: record.bundle_id.clone(),
    };
    let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
        .map_err(|_| corrupt())?;
    let [bundle] = prepared.bundles() else {
        return Err(corrupt());
    };
    let identity = bundle.identity();
    if identity.id != record.bundle_id
        || identity.version != record.version
        || identity.publisher != record.publisher
    {
        return Err(corrupt());
    }
    Ok(prepared)
}

/// What one prepared bundle contributes.
fn components(
    prepared: &PreparedCatalog,
    bundle: &PreparedInstallableBundle,
) -> BundleListingComponents {
    let id = &bundle.identity().id;
    let ids = |resources: &[hya_bundle::PreparedResource]| {
        resources
            .iter()
            .map(|resource| resource.stable_id.clone())
            .collect::<Vec<_>>()
    };
    BundleListingComponents {
        agents: bundle
            .agents()
            .iter()
            .map(|agent| agent.id.as_str().to_string())
            .collect(),
        skills: ids(bundle.skills()),
        tools: ids(bundle.tools()),
        mcp_servers: ids(bundle.mcp()),
        workflows: bundle
            .workflow()
            .map(|workflow| workflow.id.clone())
            .into_iter()
            .collect(),
        permission_modes: prepared
            .bundle_permission_modes(id)
            .iter()
            .map(|mode| format!("{id}/{}", mode.id))
            .collect(),
        apis: prepared
            .bundle_apis(id)
            .iter()
            .map(|api| format!("{} {}", api.method, api.path))
            .collect(),
        hooks: u32::try_from(bundle.hooks().len()).unwrap_or(u32::MAX),
        tui: bundle.tui().is_some(),
        tui_permissions: bundle
            .tui()
            .map(|tui| tui.permissions.clone())
            .unwrap_or_default(),
    }
}

fn listing(
    prepared: &PreparedCatalog,
    bundle: &PreparedInstallableBundle,
    scope: &str,
    state: &str,
    enabled: bool,
) -> BundleListing {
    let identity = bundle.identity();
    BundleListing {
        id: identity.id.clone(),
        version: identity.version.clone(),
        publisher: identity.publisher.clone(),
        scope: scope.to_string(),
        kind: bundle.kind().as_str().to_string(),
        state: state.to_string(),
        enabled,
        removable: scope != SCOPE_FIRST_PARTY,
        description: String::new(),
        prepared_digest: bundle.digest().to_string(),
        error: String::new(),
        components: components(prepared, bundle),
    }
}

/// Every bundle: trusted presets and first-party bundles, the user
/// registry's, and `project_dir`'s, mirroring the runtime layering (project
/// over user over first-party, by id or namespace). Sorted by id, then scope.
///
/// # Errors
/// The registry or the shipped bundles cannot be read.
pub async fn listings(
    project_dir: Option<&Path>,
) -> Result<Vec<BundleListing>, BundleControlError> {
    let registry = existing_registry().await?;
    let (disabled, installed) = match &registry {
        Some(registry) => (
            registry.disabled_bundle_ids().await.map_err(store_error)?,
            registry.snapshot().await.map_err(store_error)?.bundles,
        ),
        None => (BTreeSet::new(), Vec::new()),
    };
    let project = project_dir.map(project_bundles).unwrap_or_default();
    let mut higher_ids = BTreeSet::new();
    let mut higher_namespaces = BTreeSet::new();
    let mut rows = Vec::new();
    for bundle in &project {
        let id = bundle.bundle_id().to_string();
        let enabled = !disabled.contains(&id);
        if enabled {
            higher_ids.insert(id);
            higher_namespaces.insert(bundle.bundle().namespace().to_string());
        }
        rows.push(listing(
            bundle.prepared(),
            bundle.bundle(),
            SCOPE_PROJECT,
            if enabled { "active" } else { "disabled" },
            enabled,
        ));
    }
    for record in &installed {
        let Ok(prepared) = decode_installed_catalog(record) else {
            // Written by an incompatible version: name it and say what to do.
            rows.push(BundleListing {
                id: record.bundle_id.clone(),
                version: record.version.clone(),
                publisher: record.publisher.clone(),
                scope: SCOPE_USER.to_string(),
                state: "unreadable".to_string(),
                enabled: !disabled.contains(&record.bundle_id),
                removable: true,
                error: "unreadable registry row; reinstall the bundle".to_string(),
                ..BundleListing::default()
            });
            continue;
        };
        let [bundle] = prepared.bundles() else {
            continue;
        };
        let enabled = !disabled.contains(&record.bundle_id);
        let shadowed = higher_ids.contains(&record.bundle_id)
            || higher_namespaces.contains(bundle.namespace());
        let state = match (enabled, shadowed) {
            (false, _) => "disabled",
            (true, true) => "shadowed",
            (true, false) => "active",
        };
        rows.push(listing(&prepared, bundle, SCOPE_USER, state, enabled));
    }
    for record in &installed {
        if !disabled.contains(&record.bundle_id)
            && let Ok(prepared) = decode_installed_catalog(record)
            && let [bundle] = prepared.bundles()
        {
            higher_ids.insert(record.bundle_id.clone());
            higher_namespaces.insert(bundle.namespace().to_string());
        }
    }
    for preset in crate::trusted_preset_inventory().map_err(failure)? {
        rows.push(BundleListing {
            id: preset.id.clone(),
            version: preset.version.clone(),
            scope: SCOPE_FIRST_PARTY.to_string(),
            kind: preset.kind.clone(),
            state: "active".to_string(),
            enabled: true,
            removable: false,
            prepared_digest: preset.digest.clone(),
            components: BundleListingComponents {
                agents: preset.agent_ids.clone(),
                tools: preset.resource_ids.clone(),
                ..BundleListingComponents::default()
            },
            ..BundleListing::default()
        });
    }
    for catalog in crate::first_party_catalogs().map_err(failure)? {
        let [bundle] = catalog.bundles() else {
            continue;
        };
        let enabled = !disabled.contains(&bundle.identity().id);
        let shadowed = higher_ids.contains(&bundle.identity().id)
            || higher_namespaces.contains(bundle.namespace());
        // A first-party bundle an installed or project bundle replaces is not listed.
        if enabled && shadowed {
            continue;
        }
        let state = if enabled { "active" } else { "disabled" };
        rows.push(listing(&catalog, bundle, SCOPE_FIRST_PARTY, state, enabled));
    }
    let rank = |scope: &str| match scope {
        SCOPE_PROJECT => 0,
        SCOPE_USER => 1,
        _ => 2,
    };
    rows.sort_by(|left, right| {
        (left.id.as_str(), rank(&left.scope)).cmp(&(right.id.as_str(), rank(&right.scope)))
    });
    Ok(rows)
}

/// The server's [`BundleControl`]: the same operations `hya bundle` runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct BundleManager;

impl BundleControl for BundleManager {
    fn list(&self, project_dir: Option<PathBuf>) -> BundleControlFuture<'_, Vec<BundleListing>> {
        Box::pin(async move { listings(project_dir.as_deref()).await })
    }

    fn install(
        &self,
        package: PathBuf,
        project_dir: Option<PathBuf>,
        overwrite: bool,
    ) -> BundleControlFuture<'_, String> {
        Box::pin(async move {
            let public = inspect_installable(&package)?;
            run_self_check(&public).await?;
            let bundle_id = public
                .prepared
                .bundles()
                .first()
                .map(|bundle| bundle.identity().id.clone())
                .unwrap_or_default();
            let policy = if overwrite {
                NamespaceInstallPolicy::OverwriteConflicts
            } else {
                NamespaceInstallPolicy::DenyConflicts
            };
            match project_dir {
                Some(dir) => install(public, policy, InstallTarget::Project(&dir)).await?,
                None => {
                    let registry = open_registry().await?;
                    install(public, policy, InstallTarget::User(&registry)).await?
                }
            };
            Ok(bundle_id)
        })
    }

    fn uninstall(
        &self,
        bundle_id: String,
        project_dir: Option<PathBuf>,
    ) -> BundleControlFuture<'_, ()> {
        Box::pin(async move {
            remove(&bundle_id, project_dir.as_deref()).await?;
            Ok(())
        })
    }

    fn set_enabled(&self, bundle_id: String, enabled: bool) -> BundleControlFuture<'_, ()> {
        Box::pin(async move { set_enabled(&bundle_id, enabled).await })
    }
}
