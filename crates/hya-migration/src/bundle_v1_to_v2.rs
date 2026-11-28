//! User bundle manifest (`.hya/bundles/`) and `registry.sqlite3` v1-to-v2 migration.
//!
//! Upgrades `schema_version: 1` (or unversioned) `bundle.yaml` / `bundle.hya.md`
//! manifests to `schema_version: 2`, translates legacy `extensions.*` blocks
//! into v2 `runtime` + `assets`, detects and diagnoses removed `exposure.yaml`
//! policies and reserved kernel tools (`todo`, `rewrite`), and upgrades or
//! diagnoses rows in `registry.sqlite3`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_bundle::{BundleSource, PreparedCatalog, prepare_package};
use serde::{Deserialize, Serialize};
use serde_norway::{Mapping, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Row as _};

use crate::backup::{
    ArtifactKind, BackupTarget, atomic_swap_file, atomic_write_bytes, remove_sqlite_sidecars,
    sha256_hex, sqlite_sidecar_paths, unique_temp_path,
};
use crate::bundle_ids::{BundleIdRemap, remap_bundle_id};
use crate::error::{MigrationError, Result};

/// Target `hyabundle` manifest schema version.
pub const BUNDLE_SCHEMA_VERSION_V2: u64 = 2;

/// Reserved kernel tool names that v2 forbids bundles from declaring or aliasing.
const RESERVED_KERNEL_TOOLS: [&str; 2] = ["todo", "rewrite"];

/// Severity of a bundle migration diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    /// Informational note about an automatic transformation.
    Info,
    /// Actionable warning requiring user follow-up (e.g. `exposure.yaml` or native rebuild).
    Warning,
    /// Blocking error preventing automatic v2 validation (e.g. reserved `todo`/`rewrite` tool).
    Error,
}

/// One diagnostic message for a bundle manifest or registry entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleDiagnostic {
    /// Diagnostic severity.
    pub severity: DiagnosticSeverity,
    /// Path or bundle id the diagnostic applies to.
    pub target: String,
    /// Human-readable message and remediation guidance.
    pub message: String,
}

/// Status of one user bundle manifest directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleCheckStatus {
    /// Manifest is already valid `schema_version: 2`.
    AlreadyV2,
    /// Manifest needs v1-to-v2 upgrade.
    NeedsMigration,
}

/// Read-only inspection report for one user bundle manifest file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifestCheck {
    /// Path to `bundle.yaml` or `bundle.hya.md`.
    pub manifest_path: PathBuf,
    /// Status of the manifest.
    pub status: BundleCheckStatus,
    /// Existing `schema_version` if declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
    /// Reasons migration is needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// Diagnostics (`extensions.*`, `exposure.yaml`, reserved tools, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BundleDiagnostic>,
}

/// Status of `registry.sqlite3`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryCheckStatus {
    /// `registry.sqlite3` does not exist.
    NotFound,
    /// All installed bundles and disabled IDs are already valid v2.
    AlreadyV2,
    /// Registry contains legacy disabled IDs or unreadable/v1 bundle rows.
    NeedsMigration,
}

/// Read-only inspection report for `registry.sqlite3`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryCheckReport {
    /// Path to `registry.sqlite3`.
    pub path: PathBuf,
    /// Registry status.
    pub status: RegistryCheckStatus,
    /// Total rows in `installed_bundle`.
    pub installed_count: usize,
    /// Number of installed rows failing v2 `PreparedCatalog::decode`.
    pub unreadable_v1_rows: usize,
    /// Number of legacy bundle IDs in `bundle_disabled`.
    pub legacy_disabled_ids: usize,
    /// Diagnostics for every row requiring upgrade or reinstall.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BundleDiagnostic>,
}

/// Combined read-only check report for `.hya/bundles/` manifests and `registry.sqlite3`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlesCheckReport {
    /// Inspection result for `registry.sqlite3`.
    pub registry: RegistryCheckReport,
    /// Inspection results for every discovered manifest under `--bundles-dir`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifests: Vec<BundleManifestCheck>,
}

impl BundlesCheckReport {
    /// Whether any bundle manifest or the registry requires migration.
    #[must_use]
    pub fn needs_migration(&self) -> bool {
        self.registry.status == RegistryCheckStatus::NeedsMigration
            || self
                .manifests
                .iter()
                .any(|m| m.status == BundleCheckStatus::NeedsMigration)
    }
}

/// Planned bundle manifest rewrite.
#[derive(Clone, Debug)]
pub struct PlannedManifestWrite {
    /// Path to `bundle.yaml` or `bundle.hya.md`.
    pub manifest_path: PathBuf,
    /// Upgraded file text.
    pub new_content: String,
    /// Diagnostics emitted during upgrade.
    pub diagnostics: Vec<BundleDiagnostic>,
}

/// Full plan for upgrading user bundle manifests and `registry.sqlite3`.
#[derive(Clone, Debug)]
pub struct BundlesMigrationPlan {
    /// Path to `registry.sqlite3`.
    pub registry_path: PathBuf,
    /// Whether `registry.sqlite3` needs migration.
    pub migrate_registry: bool,
    /// Planned manifest writes.
    pub manifest_writes: Vec<PlannedManifestWrite>,
    /// Legacy `exposure.yaml` files removed so v2 `hya-bundle` accepts the bundle directory.
    pub files_to_remove: Vec<PathBuf>,
    /// Diagnostics across all inspected manifests and registry rows.
    pub diagnostics: Vec<BundleDiagnostic>,
}

impl BundlesMigrationPlan {
    /// Whether any manifest, legacy file removal, or registry migration is planned.
    #[must_use]
    pub fn needs_migration(&self) -> bool {
        self.migrate_registry
            || !self.manifest_writes.is_empty()
            || !self.files_to_remove.is_empty()
    }

    /// All files that will be modified or removed by this plan.
    #[must_use]
    pub fn backup_targets(&self) -> Vec<BackupTarget> {
        let mut out = Vec::new();
        if self.migrate_registry && self.registry_path.is_file() {
            out.push(BackupTarget {
                path: self.registry_path.clone(),
                kind: ArtifactKind::BundleRegistry,
            });
        }
        for item in &self.manifest_writes {
            out.push(BackupTarget {
                path: item.manifest_path.clone(),
                kind: ArtifactKind::BundleManifest,
            });
        }
        for path in &self.files_to_remove {
            out.push(BackupTarget {
                path: path.clone(),
                kind: ArtifactKind::BundleManifest,
            });
        }
        out
    }
}

/// Summary report returned after applying bundle migrations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundlesMigrationReport {
    /// Manifest files updated to `schema_version: 2`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifests_migrated: Vec<PathBuf>,
    /// Whether `registry.sqlite3` was modified.
    pub registry_migrated: bool,
    /// Registry disabled-bundle rows remapped.
    pub registry_disabled_remapped: usize,
    /// Registry `installed_bundle` rows upgraded in place.
    pub registry_rows_upgraded: usize,
    /// Registry `installed_bundle` rows that still require `hya bundle install`.
    pub registry_rows_needing_reinstall: usize,
    /// All diagnostics emitted during migration and post-upgrade validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BundleDiagnostic>,
}

/// Discover every `bundle.yaml` or `bundle.hya.md` under `roots`.
///
/// Each root may be a `.hya/bundles` directory containing child bundle dirs, or
/// directly a bundle directory containing `bundle.yaml` / `bundle.hya.md`.
pub fn discover_bundle_manifests(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        if root.is_file() {
            if is_manifest_file(root) && seen.insert(root.clone()) {
                found.push(root.clone());
            }
            continue;
        }
        collect_manifests_in_dir(root, &mut seen, &mut found, 0)?;
    }
    found.sort();
    Ok(found)
}

fn is_manifest_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("bundle.yaml" | "bundle.hya.md")
    )
}

fn collect_manifests_in_dir(
    dir: &Path,
    seen: &mut BTreeSet<PathBuf>,
    out: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<()> {
    if depth > 4 {
        return Ok(());
    }
    for name in ["bundle.yaml", "bundle.hya.md"] {
        let candidate = dir.join(name);
        if candidate.is_file() && seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    }
    let entries =
        fs::read_dir(dir).map_err(|err| MigrationError::io(dir, "read bundle directory", err))?;
    for entry in entries {
        let entry = entry.map_err(|err| MigrationError::io(dir, "read bundle dir entry", err))?;
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.') || n == "node_modules" || n == "target");
            if !skip {
                collect_manifests_in_dir(&path, seen, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// Inspect `registry.sqlite3` and all bundle manifests under `bundle_dirs` read-only.
pub async fn check_bundles(
    registry_path: &Path,
    bundle_dirs: &[PathBuf],
) -> Result<BundlesCheckReport> {
    let registry = check_registry(registry_path).await?;
    let manifest_paths = discover_bundle_manifests(bundle_dirs)?;
    let mut manifests = Vec::with_capacity(manifest_paths.len());
    for manifest_path in manifest_paths {
        let (_, check) = analyze_manifest(&manifest_path)?;
        manifests.push(check);
    }
    Ok(BundlesCheckReport {
        registry,
        manifests,
    })
}

/// Build the in-memory migration plan for `registry.sqlite3` and user manifests.
pub async fn plan_bundles_migration(
    registry_path: &Path,
    bundle_dirs: &[PathBuf],
) -> Result<BundlesMigrationPlan> {
    let registry = check_registry(registry_path).await?;
    let mut diagnostics = registry.diagnostics.clone();
    let manifest_paths = discover_bundle_manifests(bundle_dirs)?;
    let mut manifest_writes = Vec::new();
    let mut files_to_remove = Vec::new();
    for manifest_path in manifest_paths {
        let (planned, check) = analyze_manifest(&manifest_path)?;
        diagnostics.extend(check.diagnostics.clone());
        if let Some(write) = planned {
            manifest_writes.push(write);
        }
        if let Some(bundle_dir) = manifest_path.parent() {
            let exposure = bundle_dir.join("exposure.yaml");
            if exposure.is_file() && !files_to_remove.contains(&exposure) {
                files_to_remove.push(exposure);
            }
        }
    }
    Ok(BundlesMigrationPlan {
        registry_path: registry_path.to_path_buf(),
        migrate_registry: registry.status == RegistryCheckStatus::NeedsMigration,
        manifest_writes,
        files_to_remove,
        diagnostics,
    })
}

/// Apply a precomputed [`BundlesMigrationPlan`], atomically updating manifests
/// and `registry.sqlite3`, and validating upgraded directories with `hya-bundle`.
pub async fn apply_bundles_plan(
    plan: &BundlesMigrationPlan,
    dry_run: bool,
) -> Result<BundlesMigrationReport> {
    let mut diagnostics = plan.diagnostics.clone();
    if dry_run {
        for item in &plan.manifest_writes {
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Info,
                target: item.manifest_path.display().to_string(),
                message: "dry-run: would upgrade manifest to schema_version: 2".to_owned(),
            });
        }
        for path in &plan.files_to_remove {
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Warning,
                target: path.display().to_string(),
                message: "dry-run: would remove legacy `exposure.yaml` (backed up before removal)"
                    .to_owned(),
            });
        }
        if plan.migrate_registry {
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Info,
                target: plan.registry_path.display().to_string(),
                message: "dry-run: would migrate registry.sqlite3".to_owned(),
            });
        }
        return Ok(BundlesMigrationReport {
            manifests_migrated: Vec::new(),
            registry_migrated: false,
            registry_disabled_remapped: 0,
            registry_rows_upgraded: 0,
            registry_rows_needing_reinstall: 0,
            diagnostics,
        });
    }

    for path in &plan.files_to_remove {
        if path.is_file() {
            fs::remove_file(path)
                .map_err(|err| MigrationError::io(path, "remove legacy exposure.yaml", err))?;
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Warning,
                target: path.display().to_string(),
                message: "removed legacy `exposure.yaml` (preserved in SHA-256 backup); declare tools under `resources.tools` or `overrides: hya/bare`".to_owned(),
            });
        }
    }

    let mut manifests_migrated = Vec::new();
    for item in &plan.manifest_writes {
        atomic_write_bytes(&item.manifest_path, item.new_content.as_bytes())?;
        manifests_migrated.push(item.manifest_path.clone());

        // Validate upgraded directory against v2 `hya-bundle` parser/preparer.
        if let Some(bundle_dir) = item.manifest_path.parent() {
            match BundleSource::read_directory(bundle_dir).and_then(prepare_package) {
                Ok(_) => {
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        target: item.manifest_path.display().to_string(),
                        message: "validated upgraded bundle with v2 hya-bundle prepare_package"
                            .to_owned(),
                    });
                }
                Err(err) => {
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        target: item.manifest_path.display().to_string(),
                        message: format!(
                            "upgraded manifest to schema_version: 2, but v2 prepare_package reported: {err}"
                        ),
                    });
                }
            }
        }
    }

    let mut registry_migrated = false;
    let mut registry_disabled_remapped = 0;
    let mut registry_rows_upgraded = 0;
    let mut registry_rows_needing_reinstall = 0;
    if plan.migrate_registry && plan.registry_path.is_file() {
        let reg = migrate_registry(&plan.registry_path).await?;
        registry_migrated = reg.migrated;
        registry_disabled_remapped = reg.disabled_remapped;
        registry_rows_upgraded = reg.rows_upgraded;
        registry_rows_needing_reinstall = reg.rows_needing_reinstall;
        diagnostics.extend(reg.diagnostics);
    }

    Ok(BundlesMigrationReport {
        manifests_migrated,
        registry_migrated,
        registry_disabled_remapped,
        registry_rows_upgraded,
        registry_rows_needing_reinstall,
        diagnostics,
    })
}

// ---------------------------------------------------------------------------
// Manifest (`bundle.yaml` / `bundle.hya.md`) analysis & upgrade
// ---------------------------------------------------------------------------

fn analyze_manifest(
    manifest_path: &Path,
) -> Result<(Option<PlannedManifestWrite>, BundleManifestCheck)> {
    let raw = fs::read_to_string(manifest_path)
        .map_err(|err| MigrationError::io(manifest_path, "read bundle manifest", err))?;
    let is_markdown = manifest_path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "bundle.hya.md");

    let (yaml_text, markdown_body) = if is_markdown {
        let Some((fm, body)) = split_markdown(&raw) else {
            return Err(MigrationError::InvalidArtifact {
                path: manifest_path.to_path_buf(),
                detail: "bundle.hya.md requires YAML frontmatter delimited by `---`".to_owned(),
            });
        };
        (fm, Some(body))
    } else {
        (raw.as_str(), None)
    };

    let val: Value = serde_norway::from_str(yaml_text).map_err(|source| MigrationError::Yaml {
        context: format!("parse {}", manifest_path.display()),
        source,
    })?;
    let Some(mut map) = val.as_mapping().cloned() else {
        return Err(MigrationError::InvalidArtifact {
            path: manifest_path.to_path_buf(),
            detail: "bundle manifest must be a YAML mapping".to_owned(),
        });
    };

    let target_label = manifest_path.display().to_string();
    let schema_version = map
        .get(Value::String("schema_version".to_owned()))
        .and_then(Value::as_u64);

    let mut reasons = Vec::new();
    let mut diagnostics = Vec::new();
    let mut changed = false;

    if schema_version != Some(BUNDLE_SCHEMA_VERSION_V2) {
        let found = schema_version.map_or_else(|| "missing".to_owned(), |v| v.to_string());
        reasons.push(format!(
            "manifest `schema_version` is `{found}` (v2 requires `2`)"
        ));
        changed = true;
    }

    // Normalize legacy `version_ref: backend` -> `version_ref: hya`.
    if map
        .get(Value::String("version_ref".to_owned()))
        .and_then(Value::as_str)
        == Some("backend")
    {
        map.insert(
            Value::String("version_ref".to_owned()),
            Value::String("hya".to_owned()),
        );
        reasons.push("normalized `version_ref: backend` -> `version_ref: hya`".to_owned());
        changed = true;
    }

    // Check for sibling `exposure.yaml` on disk (rejected by v2 hya-bundle).
    if let Some(bundle_dir) = manifest_path.parent()
        && bundle_dir.join("exposure.yaml").is_file()
    {
        reasons.push("legacy `exposure.yaml` file present in bundle directory".to_owned());
        diagnostics.push(BundleDiagnostic {
            severity: DiagnosticSeverity::Warning,
            target: bundle_dir.join("exposure.yaml").display().to_string(),
            message: "legacy `exposure.yaml` is no longer supported in v2 (ADR-0039); use `overrides: hya/bare` on the bundle or individual tools instead".to_owned(),
        });
    }

    // Upgrade legacy `extensions:` block into `runtime:` and `assets:`.
    if let Some(ext_val) = map.remove(Value::String("extensions".to_owned())) {
        changed = true;
        reasons.push("migrated legacy `extensions:` block to v2 `runtime:` / `assets:`".to_owned());
        migrate_extensions_block(&mut map, ext_val, &target_label, &mut diagnostics);
    }

    // Check `assets:` for legacy `exposure.yaml` entries and remove them with a diagnostic.
    if let Some(assets_val) = map.get_mut(Value::String("assets".to_owned()))
        && let Some(assets_seq) = assets_val.as_sequence_mut()
    {
        let before = assets_seq.len();
        assets_seq.retain(|item| !is_exposure_resource(item));
        if assets_seq.len() < before {
            changed = true;
            reasons.push("removed legacy `exposure.yaml` entry from `assets`".to_owned());
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Warning,
                target: target_label.clone(),
                message: "removed `exposure.yaml` reference from `assets`; BaseToolScheme/exposure policies are replaced by `overrides: hya/bare` in v2".to_owned(),
            });
        }
    }

    // Check for reserved kernel tools (`todo`, `rewrite`) in `resources.tools`.
    check_reserved_tools(&map, &target_label, &mut diagnostics);

    // Check `runtime.abi_version: 1` -> bump diagnostic to 2.
    if let Some(runtime_map) = map
        .get_mut(Value::String("runtime".to_owned()))
        .and_then(Value::as_mapping_mut)
        && runtime_map
            .get(Value::String("abi_version".to_owned()))
            .and_then(Value::as_u64)
            == Some(1)
    {
        runtime_map.insert(
            Value::String("abi_version".to_owned()),
            Value::Number(2u64.into()),
        );
        changed = true;
        reasons.push("bumped `runtime.abi_version` from `1` to `2`".to_owned());
        diagnostics.push(BundleDiagnostic {
            severity: DiagnosticSeverity::Warning,
            target: target_label.clone(),
            message: "native bundle declared `abi_version: 1`; updated to `abi_version: 2` — rebuild the native library against `hya-runtime-abi` v2".to_owned(),
        });
    }

    if !changed {
        return Ok((
            None,
            BundleManifestCheck {
                manifest_path: manifest_path.to_path_buf(),
                status: BundleCheckStatus::AlreadyV2,
                schema_version,
                reasons,
                diagnostics,
            },
        ));
    }

    // Rebuild YAML with `kind` followed by `schema_version: 2`.
    let kind_val = map.remove(Value::String("kind".to_owned()));
    map.remove(Value::String("schema_version".to_owned()));
    let mut ordered = Mapping::with_capacity(map.len() + 2);
    if let Some(k) = kind_val {
        ordered.insert(Value::String("kind".to_owned()), k);
    }
    ordered.insert(
        Value::String("schema_version".to_owned()),
        Value::Number(BUNDLE_SCHEMA_VERSION_V2.into()),
    );
    for (k, v) in map {
        ordered.insert(k, v);
    }

    let rendered_yaml = serde_norway::to_string(&Value::Mapping(ordered)).map_err(|source| {
        MigrationError::Yaml {
            context: format!("serialize upgraded {}", manifest_path.display()),
            source,
        }
    })?;
    let new_content = if let Some(body) = markdown_body {
        format!("---\n{rendered_yaml}---\n{body}")
    } else {
        rendered_yaml
    };

    let check = BundleManifestCheck {
        manifest_path: manifest_path.to_path_buf(),
        status: BundleCheckStatus::NeedsMigration,
        schema_version,
        reasons,
        diagnostics: diagnostics.clone(),
    };
    let planned = PlannedManifestWrite {
        manifest_path: manifest_path.to_path_buf(),
        new_content,
        diagnostics,
    };
    Ok((Some(planned), check))
}

fn migrate_extensions_block(
    manifest: &mut Mapping,
    ext_val: Value,
    target_label: &str,
    diagnostics: &mut Vec<BundleDiagnostic>,
) {
    let Some(ext) = ext_val.as_mapping() else {
        diagnostics.push(BundleDiagnostic {
            severity: DiagnosticSeverity::Warning,
            target: target_label.to_owned(),
            message: "dropped malformed non-mapping `extensions` block".to_owned(),
        });
        return;
    };

    let mut new_assets: Vec<Value> = manifest
        .remove(Value::String("assets".to_owned()))
        .and_then(|v| v.as_sequence().cloned())
        .unwrap_or_default();

    // 1. `extensions.files` -> `assets` (excluding `exposure.yaml`).
    if let Some(files) = ext
        .get(Value::String("files".to_owned()))
        .and_then(Value::as_sequence)
    {
        for item in files {
            if is_exposure_resource(item) {
                diagnostics.push(BundleDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    target: target_label.to_owned(),
                    message: "removed `exposure.yaml` from `extensions.files`; v2 replaces exposure policies with `overrides: hya/bare`".to_owned(),
                });
            } else {
                new_assets.push(item.clone());
            }
        }
    }

    let js_items = ext
        .get(Value::String("js".to_owned()))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let rust_items = ext
        .get(Value::String("rust".to_owned()))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let lib_items = ext
        .get(Value::String("libraries".to_owned()))
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    let process_map = ext
        .get(Value::String("process".to_owned()))
        .and_then(Value::as_mapping);

    let has_runtime = manifest.contains_key(Value::String("runtime".to_owned()));

    // 2. Translate `extensions.process` / `extensions.js` / `extensions.libraries`.
    if let Some(proc) = process_map {
        let kind = proc
            .get(Value::String("kind".to_owned()))
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "bun" => {
                if !has_runtime {
                    let mut rt = Mapping::new();
                    rt.insert(
                        Value::String("kind".to_owned()),
                        Value::String("js".to_owned()),
                    );
                    rt.insert(
                        Value::String("mode".to_owned()),
                        Value::String("process".to_owned()),
                    );
                    if let Some(entry_path) = js_items.first().and_then(resource_path) {
                        rt.insert(
                            Value::String("entry".to_owned()),
                            Value::String(entry_path.to_owned()),
                        );
                    }
                    if let Some(cmd) = proc.get(Value::String("command".to_owned())) {
                        rt.insert(Value::String("command".to_owned()), cmd.clone());
                    }
                    if let Some(timeout) = proc.get(Value::String("timeout_ms".to_owned())) {
                        rt.insert(Value::String("timeout_ms".to_owned()), timeout.clone());
                    }
                    manifest.insert(Value::String("runtime".to_owned()), Value::Mapping(rt));
                    for item in js_items.into_iter().skip(1) {
                        new_assets.push(item);
                    }
                } else {
                    for item in js_items {
                        new_assets.push(item);
                    }
                }
            }
            "claude" => {
                if !has_runtime {
                    let mut rt = Mapping::new();
                    rt.insert(
                        Value::String("kind".to_owned()),
                        Value::String("js".to_owned()),
                    );
                    rt.insert(
                        Value::String("mode".to_owned()),
                        Value::String("claude".to_owned()),
                    );
                    if let Some(cmd) = proc.get(Value::String("command".to_owned())) {
                        rt.insert(Value::String("command".to_owned()), cmd.clone());
                    }
                    if let Some(timeout) = proc.get(Value::String("timeout_ms".to_owned())) {
                        rt.insert(Value::String("timeout_ms".to_owned()), timeout.clone());
                    }
                    manifest.insert(Value::String("runtime".to_owned()), Value::Mapping(rt));
                }
            }
            "rust" => {
                diagnostics.push(BundleDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    target: target_label.to_owned(),
                    message: "removed legacy `extensions.process` (`kind: rust`); port to `runtime: { kind: native, abi_version: 2 }` (`hya-runtime-abi`) or `runtime: { kind: js, mode: process }`".to_owned(),
                });
            }
            other => {
                diagnostics.push(BundleDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    target: target_label.to_owned(),
                    message: format!("removed unrecognized `extensions.process.kind: {other}`"),
                });
            }
        }
    } else if !js_items.is_empty() {
        if !has_runtime && let Some(entry_path) = js_items.first().and_then(resource_path) {
            let mut rt = Mapping::new();
            rt.insert(
                Value::String("kind".to_owned()),
                Value::String("js".to_owned()),
            );
            rt.insert(
                Value::String("mode".to_owned()),
                Value::String("managed".to_owned()),
            );
            rt.insert(
                Value::String("entry".to_owned()),
                Value::String(entry_path.to_owned()),
            );
            manifest.insert(Value::String("runtime".to_owned()), Value::Mapping(rt));
            // Additional JS modules beyond the entry file stay packaged as assets.
            for item in js_items.into_iter().skip(1) {
                new_assets.push(item);
            }
        } else {
            for item in js_items {
                new_assets.push(item);
            }
        }
    } else if !lib_items.is_empty() {
        if !has_runtime && let Some(lib_path) = lib_items.first().and_then(resource_path) {
            let mut rt = Mapping::new();
            rt.insert(
                Value::String("kind".to_owned()),
                Value::String("native".to_owned()),
            );
            rt.insert(
                Value::String("abi_version".to_owned()),
                Value::Number(2u64.into()),
            );
            rt.insert(
                Value::String("library".to_owned()),
                Value::String(lib_path.to_owned()),
            );
            rt.insert(
                Value::String("capabilities".to_owned()),
                Value::Sequence(vec![Value::String("tools".to_owned())]),
            );
            manifest.insert(Value::String("runtime".to_owned()), Value::Mapping(rt));
        }
        diagnostics.push(BundleDiagnostic {
            severity: DiagnosticSeverity::Warning,
            target: target_label.to_owned(),
            message: "migrated `extensions.libraries` to `runtime: { kind: native, abi_version: 2 }`; rebuild the dynamic library against `hya-runtime-abi` v2".to_owned(),
        });
    }

    if !rust_items.is_empty() {
        diagnostics.push(BundleDiagnostic {
            severity: DiagnosticSeverity::Warning,
            target: target_label.to_owned(),
            message: "removed legacy `extensions.rust` executables; migrate to `runtime: { kind: native, abi_version: 2 }`".to_owned(),
        });
    }

    if !new_assets.is_empty() {
        manifest.insert(
            Value::String("assets".to_owned()),
            Value::Sequence(new_assets),
        );
    }
}

fn resource_path(value: &Value) -> Option<&str> {
    value
        .as_mapping()
        .and_then(|m| m.get(Value::String("path".to_owned())))
        .and_then(Value::as_str)
}

fn is_exposure_resource(value: &Value) -> bool {
    let Some(map) = value.as_mapping() else {
        return false;
    };
    let id_is_exposure = map
        .get(Value::String("id".to_owned()))
        .and_then(Value::as_str)
        == Some("exposure");
    let path_is_exposure = map
        .get(Value::String("path".to_owned()))
        .and_then(Value::as_str)
        .is_some_and(|p| p == "exposure.yaml" || p.ends_with("/exposure.yaml"));
    id_is_exposure || path_is_exposure
}

fn check_reserved_tools(
    manifest: &Mapping,
    target_label: &str,
    diagnostics: &mut Vec<BundleDiagnostic>,
) {
    let Some(tools) = manifest
        .get(Value::String("resources".to_owned()))
        .and_then(Value::as_mapping)
        .and_then(|r| r.get(Value::String("tools".to_owned())))
        .and_then(Value::as_sequence)
    else {
        return;
    };
    for tool in tools {
        let Some(map) = tool.as_mapping() else {
            continue;
        };
        if let Some(id) = map
            .get(Value::String("id".to_owned()))
            .and_then(Value::as_str)
            && RESERVED_KERNEL_TOOLS.contains(&id)
        {
            diagnostics.push(BundleDiagnostic {
                severity: DiagnosticSeverity::Error,
                target: target_label.to_owned(),
                message: format!(
                    "tool `{id}` is permanently reserved by hya-core in v2 (ADR-0039) and must be renamed or removed"
                ),
            });
        }
        if let Some(aliases) = map
            .get(Value::String("aliases".to_owned()))
            .and_then(Value::as_sequence)
        {
            for alias in aliases.iter().filter_map(Value::as_str) {
                if RESERVED_KERNEL_TOOLS.contains(&alias) {
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Error,
                        target: target_label.to_owned(),
                        message: format!(
                            "tool alias `{alias}` is permanently reserved by hya-core in v2 (ADR-0039) and must be removed"
                        ),
                    });
                }
            }
        }
    }
}

fn split_markdown(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let (frontmatter, body) = rest.split_once("\n---")?;
    Some((
        frontmatter.strip_suffix('\r').unwrap_or(frontmatter),
        body.strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body),
    ))
}

// ---------------------------------------------------------------------------
// `registry.sqlite3` inspection & migration
// ---------------------------------------------------------------------------

struct RegistryStagingCopy {
    dir: PathBuf,
    db_path: PathBuf,
}

impl RegistryStagingCopy {
    fn create(source: &Path) -> Result<Self> {
        let temp_name = unique_temp_path(source, "regstage")?;
        let leaf = temp_name
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("hya-reg-stage");
        let dir = std::env::temp_dir().join(format!("hya-migration-{leaf}"));
        fs::create_dir_all(&dir)
            .map_err(|err| MigrationError::io(&dir, "create staging registry dir", err))?;
        let dst = dir.join("registry.sqlite3");
        fs::copy(source, &dst)
            .map_err(|err| MigrationError::io(source, "copy registry.sqlite3 to staging", err))?;
        let [src_wal, src_shm] = sqlite_sidecar_paths(source);
        let [dst_wal, dst_shm] = sqlite_sidecar_paths(&dst);
        if src_wal.exists() {
            fs::copy(&src_wal, &dst_wal)
                .map_err(|err| MigrationError::io(&src_wal, "copy registry wal", err))?;
        }
        if src_shm.exists() {
            fs::copy(&src_shm, &dst_shm)
                .map_err(|err| MigrationError::io(&src_shm, "copy registry shm", err))?;
        }
        Ok(Self { dir, db_path: dst })
    }
}

impl Drop for RegistryStagingCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn check_registry(path: &Path) -> Result<RegistryCheckReport> {
    if !path.is_file() {
        return Ok(RegistryCheckReport {
            path: path.to_path_buf(),
            status: RegistryCheckStatus::NotFound,
            installed_count: 0,
            unreadable_v1_rows: 0,
            legacy_disabled_ids: 0,
            diagnostics: Vec::new(),
        });
    }

    let staging = RegistryStagingCopy::create(path)?;
    let opts = SqliteConnectOptions::new()
        .filename(&staging.db_path)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5));
    let mut conn =
        SqliteConnection::connect_with(&opts)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: format!("open staging copy of {}", path.display()),
                source: err,
            })?;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut conn)
    .await
    .map_err(|err| MigrationError::Sqlx {
        context: "list registry tables".to_owned(),
        source: err,
    })?;
    let has_table = |name: &str| tables.iter().any(|t| t == name);

    let mut diagnostics = Vec::new();
    let mut installed_count = 0usize;
    let mut unreadable_v1_rows = 0usize;

    if has_table("installed_bundle") {
        let rows = sqlx::query(
            "SELECT bundle_id, version, prepared_digest, prepared_bytes FROM installed_bundle ORDER BY bundle_id",
        )
        .fetch_all(&mut conn)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read installed_bundle".to_owned(),
            source: err,
        })?;
        installed_count = rows.len();
        for row in rows {
            let bundle_id: String =
                row.try_get("bundle_id")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read installed_bundle.bundle_id".to_owned(),
                        source: err,
                    })?;
            let version: String = row.try_get("version").map_err(|err| MigrationError::Sqlx {
                context: "read installed_bundle.version".to_owned(),
                source: err,
            })?;
            let prepared_digest: String =
                row.try_get("prepared_digest")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read installed_bundle.prepared_digest".to_owned(),
                        source: err,
                    })?;
            let prepared_bytes: Vec<u8> =
                row.try_get("prepared_bytes")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read installed_bundle.prepared_bytes".to_owned(),
                        source: err,
                    })?;

            match PreparedCatalog::decode(&prepared_bytes, &prepared_digest) {
                Ok(_) => {}
                Err(err) => {
                    unreadable_v1_rows += 1;
                    let upgradeable = try_upgrade_prepared_catalog_bytes(&prepared_bytes).is_some();
                    let detail = if upgradeable {
                        format!(
                            "installed bundle `{bundle_id}@{version}` uses v1 prepared format ({err}); will be upgraded automatically"
                        )
                    } else {
                        format!(
                            "installed bundle `{bundle_id}@{version}` is incompatible with v2 ({err}); reinstall with `hya bundle install`"
                        )
                    };
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Warning,
                        target: format!("{}::{bundle_id}", path.display()),
                        message: detail,
                    });
                }
            }
        }
    }

    let mut legacy_disabled_ids = 0usize;
    if has_table("bundle_disabled") {
        let disabled: Vec<String> =
            sqlx::query_scalar("SELECT bundle_id FROM bundle_disabled ORDER BY bundle_id")
                .fetch_all(&mut conn)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "read bundle_disabled".to_owned(),
                    source: err,
                })?;
        for id in disabled {
            match remap_bundle_id(&id) {
                BundleIdRemap::Remapped { from, to } => {
                    legacy_disabled_ids += 1;
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        target: format!("{}::bundle_disabled", path.display()),
                        message: format!(
                            "legacy disabled bundle `{from}` will remap to [{}]",
                            to.join(", ")
                        ),
                    });
                }
                BundleIdRemap::PromotedToCore { from, note } => {
                    legacy_disabled_ids += 1;
                    diagnostics.push(BundleDiagnostic {
                        severity: DiagnosticSeverity::Info,
                        target: format!("{}::bundle_disabled", path.display()),
                        message: format!(
                            "legacy disabled bundle `{from}` will be removed ({note})"
                        ),
                    });
                }
                BundleIdRemap::Unchanged(_) => {}
            }
        }
    }

    let _ = conn.close().await;

    let status = if unreadable_v1_rows > 0 || legacy_disabled_ids > 0 {
        RegistryCheckStatus::NeedsMigration
    } else {
        RegistryCheckStatus::AlreadyV2
    };

    Ok(RegistryCheckReport {
        path: path.to_path_buf(),
        status,
        installed_count,
        unreadable_v1_rows,
        legacy_disabled_ids,
        diagnostics,
    })
}

struct RegistryMigrationOutcome {
    migrated: bool,
    disabled_remapped: usize,
    rows_upgraded: usize,
    rows_needing_reinstall: usize,
    diagnostics: Vec<BundleDiagnostic>,
}

async fn migrate_registry(path: &Path) -> Result<RegistryMigrationOutcome> {
    let temp_target = unique_temp_path(path, "v2reg")?;
    let outcome = run_registry_migration(path, &temp_target).await;
    match outcome {
        Ok(res) => {
            if res.migrated {
                atomic_swap_file(&temp_target, path)?;
                remove_sqlite_sidecars(path)?;
            } else {
                let _ = fs::remove_file(&temp_target);
                let _ = remove_sqlite_sidecars(&temp_target);
            }
            Ok(res)
        }
        Err(err) => {
            let _ = fs::remove_file(&temp_target);
            let _ = remove_sqlite_sidecars(&temp_target);
            Err(err)
        }
    }
}

async fn run_registry_migration(
    source_path: &Path,
    temp_target: &Path,
) -> Result<RegistryMigrationOutcome> {
    // Copy source registry (and WAL/SHM) to `temp_target` so mutation is atomic.
    fs::copy(source_path, temp_target)
        .map_err(|err| MigrationError::io(source_path, "copy registry to temp file", err))?;
    let [src_wal, src_shm] = sqlite_sidecar_paths(source_path);
    let [dst_wal, dst_shm] = sqlite_sidecar_paths(temp_target);
    if src_wal.exists() {
        fs::copy(&src_wal, &dst_wal)
            .map_err(|err| MigrationError::io(&src_wal, "copy registry wal to temp", err))?;
    }
    if src_shm.exists() {
        fs::copy(&src_shm, &dst_shm)
            .map_err(|err| MigrationError::io(&src_shm, "copy registry shm to temp", err))?;
    }

    let opts = SqliteConnectOptions::new()
        .filename(temp_target)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5));
    let mut conn =
        SqliteConnection::connect_with(&opts)
            .await
            .map_err(|err| MigrationError::Sqlx {
                context: format!("open temp registry {}", temp_target.display()),
                source: err,
            })?;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut conn)
    .await
    .map_err(|err| MigrationError::Sqlx {
        context: "list temp registry tables".to_owned(),
        source: err,
    })?;
    let has_table = |name: &str| tables.iter().any(|t| t == name);

    let mut tx = conn.begin().await.map_err(|err| MigrationError::Sqlx {
        context: "begin registry migration tx".to_owned(),
        source: err,
    })?;

    let mut disabled_remapped = 0usize;
    let mut rows_upgraded = 0usize;
    let mut rows_needing_reinstall = 0usize;
    let mut diagnostics = Vec::new();

    if has_table("bundle_disabled") {
        let disabled: Vec<String> =
            sqlx::query_scalar("SELECT bundle_id FROM bundle_disabled ORDER BY bundle_id")
                .fetch_all(&mut *tx)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: "read bundle_disabled".to_owned(),
                    source: err,
                })?;
        for id in disabled {
            match remap_bundle_id(&id) {
                BundleIdRemap::Remapped { from, to } => {
                    sqlx::query("DELETE FROM bundle_disabled WHERE bundle_id = ?")
                        .bind(&from)
                        .execute(&mut *tx)
                        .await
                        .map_err(|err| MigrationError::Sqlx {
                            context: format!("delete legacy disabled `{from}`"),
                            source: err,
                        })?;
                    for target in to {
                        sqlx::query("INSERT OR IGNORE INTO bundle_disabled (bundle_id) VALUES (?)")
                            .bind(target)
                            .execute(&mut *tx)
                            .await
                            .map_err(|err| MigrationError::Sqlx {
                                context: format!("insert remapped disabled `{target}`"),
                                source: err,
                            })?;
                    }
                    disabled_remapped += 1;
                }
                BundleIdRemap::PromotedToCore { from, .. } => {
                    sqlx::query("DELETE FROM bundle_disabled WHERE bundle_id = ?")
                        .bind(&from)
                        .execute(&mut *tx)
                        .await
                        .map_err(|err| MigrationError::Sqlx {
                            context: format!("delete promoted disabled `{from}`"),
                            source: err,
                        })?;
                    disabled_remapped += 1;
                }
                BundleIdRemap::Unchanged(_) => {}
            }
        }
    }

    if has_table("installed_bundle") {
        let rows = sqlx::query(
            "SELECT bundle_id, version, prepared_digest, prepared_bytes FROM installed_bundle ORDER BY bundle_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "read installed_bundle for upgrade".to_owned(),
            source: err,
        })?;
        for row in rows {
            let bundle_id: String =
                row.try_get("bundle_id")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read bundle_id".to_owned(),
                        source: err,
                    })?;
            let version: String = row.try_get("version").map_err(|err| MigrationError::Sqlx {
                context: "read version".to_owned(),
                source: err,
            })?;
            let prepared_digest: String =
                row.try_get("prepared_digest")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read prepared_digest".to_owned(),
                        source: err,
                    })?;
            let prepared_bytes: Vec<u8> =
                row.try_get("prepared_bytes")
                    .map_err(|err| MigrationError::Sqlx {
                        context: "read prepared_bytes".to_owned(),
                        source: err,
                    })?;

            if PreparedCatalog::decode(&prepared_bytes, &prepared_digest).is_ok() {
                continue;
            }

            if let Some((upgraded_bytes, upgraded_digest)) =
                try_upgrade_prepared_catalog_bytes(&prepared_bytes)
            {
                sqlx::query(
                    "UPDATE installed_bundle SET prepared_digest = ?, prepared_bytes = ? WHERE bundle_id = ?",
                )
                .bind(&upgraded_digest)
                .bind(&upgraded_bytes)
                .bind(&bundle_id)
                .execute(&mut *tx)
                .await
                .map_err(|err| MigrationError::Sqlx {
                    context: format!("update upgraded installed_bundle `{bundle_id}`"),
                    source: err,
                })?;
                rows_upgraded += 1;
                diagnostics.push(BundleDiagnostic {
                    severity: DiagnosticSeverity::Info,
                    target: format!("{}::{bundle_id}", source_path.display()),
                    message: format!(
                        "upgraded installed bundle `{bundle_id}@{version}` prepared catalog to v2"
                    ),
                });
            } else {
                rows_needing_reinstall += 1;
                diagnostics.push(BundleDiagnostic {
                    severity: DiagnosticSeverity::Warning,
                    target: format!("{}::{bundle_id}", source_path.display()),
                    message: format!(
                        "installed bundle `{bundle_id}@{version}` could not be upgraded automatically; run `hya bundle install` with a v2 package"
                    ),
                });
            }
        }
    }

    let mutated = disabled_remapped > 0 || rows_upgraded > 0;
    if mutated && has_table("bundle_registry_generation") {
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *tx)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "bump bundle_registry_generation".to_owned(),
            source: err,
        })?;
    }

    tx.commit().await.map_err(|err| MigrationError::Sqlx {
        context: "commit registry migration tx".to_owned(),
        source: err,
    })?;

    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut conn)
        .await
        .map_err(|err| MigrationError::Sqlx {
            context: "registry PRAGMA integrity_check".to_owned(),
            source: err,
        })?;
    if integrity != ["ok"] {
        return Err(MigrationError::DatabaseIntegrity {
            path: source_path.to_path_buf(),
            detail: format!("PRAGMA integrity_check returned: {}", integrity.join("; ")),
        });
    }

    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut conn)
        .await;
    let _ = sqlx::query("PRAGMA journal_mode = DELETE")
        .execute(&mut conn)
        .await;
    let _ = conn.close().await;
    let _ = remove_sqlite_sidecars(temp_target);

    Ok(RegistryMigrationOutcome {
        migrated: mutated,
        disabled_remapped,
        rows_upgraded,
        rows_needing_reinstall,
        diagnostics,
    })
}

/// Attempt to upgrade a v1 `PreparedCatalog` JSON blob in `registry.sqlite3` into
/// a valid v2 `PreparedCatalog` (re-encoded with recomputed bundle & catalog
/// SHA-256 digests and verified via [`PreparedCatalog::decode`]).
fn try_upgrade_prepared_catalog_bytes(raw: &[u8]) -> Option<(Vec<u8>, String)> {
    const TARGET_PREPARED_FORMAT_VERSION: u64 = 4;

    let mut doc: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let obj = doc.as_object_mut()?;

    if let Some(fv) = obj
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        && fv < TARGET_PREPARED_FORMAT_VERSION
    {
        obj.insert(
            "format_version".to_owned(),
            serde_json::Value::Number(TARGET_PREPARED_FORMAT_VERSION.into()),
        );
    }
    obj.remove("schema_version");

    if let Some(exts) = obj
        .get_mut("extensions")
        .and_then(serde_json::Value::as_array_mut)
    {
        exts.retain(|res| {
            let source_path = res
                .get("source_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            source_path != "exposure.yaml" && !source_path.ends_with("/exposure.yaml")
        });
    }

    let mut bundle: hya_bundle::Bundle = serde_json::from_value(doc).ok()?;
    let mut bundle_val = serde_json::to_value(&bundle).ok()?;
    let bundle_fields = bundle_val.as_object_mut()?;
    bundle_fields.remove("digest");
    let canonical_without_digest = serde_json::to_vec(&bundle_val).ok()?;
    bundle.digest = sha256_hex(&canonical_without_digest);

    let encoded = serde_json::to_vec(&bundle).ok()?;
    let digest = sha256_hex(&encoded);
    if PreparedCatalog::decode(&encoded, &digest).is_ok() {
        return Some((encoded, digest));
    }
    None
}
