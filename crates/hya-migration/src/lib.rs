//! Standalone v1-to-v2 migration library for `hya` (ADR-0039).
//!
//! Covers offline inspection (`check`), atomic SHA-256-backed migration (`run`),
//! and SHA-256-verified byte-exact restoration (`rollback`) for:
//! - SQLite session database (`sessions.db`)
//! - Hya configuration (`config.yaml` + per-bundle `config.yml`)
//! - TUI configuration and saved workspace layout (`tui.toml`, `tui.json`)
//! - User bundle manifests (`.hya/bundles/`) and installed registry (`registry.sqlite3`)

pub mod backup;
pub mod bundle_ids;
pub mod bundle_v1_to_v2;
pub mod config_v1_to_v2;
pub mod db_v1_to_v2;
pub mod error;
pub mod tui_v1_to_v2;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backup::{
    ArtifactKind, BackupManifest, BackupTarget, RollbackReport, create_backup_snapshot,
    rollback_from_backup,
};
use crate::bundle_v1_to_v2::{
    BundlesCheckReport, BundlesMigrationReport, apply_bundles_plan, check_bundles,
    plan_bundles_migration,
};
use crate::config_v1_to_v2::{
    ConfigCheckReport, ConfigCheckStatus, ConfigMigrationReport, apply_config_plan, check_config,
    plan_config_migration,
};
use crate::db_v1_to_v2::{DbCheckReport, DbCheckStatus, DbMigrationReport, check_db, migrate_db};
use crate::error::Result;
use crate::tui_v1_to_v2::{
    TuiCheckReport, TuiMigrationReport, apply_tui_plan, check_tui, plan_tui_migration,
};

/// Paths inspected and migrated by `hya-migration`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationTargets {
    /// Path to `sessions.db`.
    pub db: PathBuf,
    /// Path to `config.yaml`.
    pub config: PathBuf,
    /// Path to `tui.toml`.
    pub tui_toml: PathBuf,
    /// Path to `tui.json` (holds saved `paneLayout`, `extensionEnabled`, etc.).
    pub tui_json: PathBuf,
    /// Path to `registry.sqlite3`.
    pub registry: PathBuf,
    /// User/project bundle directories (e.g. `.hya/bundles/`).
    pub bundles_dirs: Vec<PathBuf>,
}

impl MigrationTargets {
    /// Resolve default target paths from standard XDG / `HYA_*` / `HOME` environment variables.
    #[must_use]
    pub fn resolve_defaults() -> Self {
        let config_dir = default_config_dir();
        let state_dir = default_state_dir();
        let data_dir = default_data_dir();

        let config = config_dir.join("config.yaml");
        let tui_json = std::env::var_os("HYA_TUI_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("tui.json"));
        let tui_toml = std::env::var_os("HYA_TUI_TOML")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                tui_json
                    .parent()
                    .map_or_else(|| config_dir.join("tui.toml"), |p| p.join("tui.toml"))
            });
        let db = state_dir.join("sessions.db");
        let registry = data_dir.join("bundles").join("registry.sqlite3");

        let mut bundles_dirs = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            let local_bundles = cwd.join(".hya").join("bundles");
            if local_bundles.is_dir() {
                bundles_dirs.push(local_bundles);
            }
        }

        Self {
            db,
            config,
            tui_toml,
            tui_json,
            registry,
            bundles_dirs,
        }
    }
}

/// Combined read-only report produced by [`check_all`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    /// Whether any inspected artifact still needs v1-to-v2 migration.
    pub needs_migration: bool,
    /// `sessions.db` inspection.
    pub db: DbCheckReport,
    /// `config.yaml` inspection.
    pub config: ConfigCheckReport,
    /// `tui.toml` and `tui.json` inspection.
    pub tui: TuiCheckReport,
    /// `registry.sqlite3` and `.hya/bundles/` inspection.
    pub bundles: BundlesCheckReport,
}

/// Options controlling [`run_migration`].
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// When `true`, plan and report every change without writing any file.
    pub dry_run: bool,
    /// Explicit backup snapshot directory. Defaults to a timestamped directory
    /// beside `config.yaml` when omitted.
    pub backup_dir: Option<PathBuf>,
}

/// Combined report produced by [`run_migration`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    /// Whether this invocation ran in `--dry-run` mode.
    pub dry_run: bool,
    /// Directory where the SHA-256 backup manifest and snapshots were stored
    /// (`None` on dry-run or when nothing needed migration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<PathBuf>,
    /// Backup manifest written prior to mutation, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_manifest: Option<BackupManifest>,
    /// `sessions.db` migration result.
    pub db: DbMigrationReport,
    /// `config.yaml` migration result.
    pub config: ConfigMigrationReport,
    /// `tui.toml` / `tui.json` migration result.
    pub tui: TuiMigrationReport,
    /// `.hya/bundles/` and `registry.sqlite3` migration result.
    pub bundles: BundlesMigrationReport,
}

/// Inspect all configured artifacts without modifying any file on disk.
pub async fn check_all(targets: &MigrationTargets) -> Result<CheckReport> {
    let db = check_db(&targets.db).await?;
    let config = check_config(&targets.config)?;
    let tui = check_tui(&targets.tui_toml, &targets.tui_json)?;
    let bundles = check_bundles(&targets.registry, &targets.bundles_dirs).await?;

    let needs_migration = db.status == DbCheckStatus::NeedsMigration
        || config.status == ConfigCheckStatus::NeedsMigration
        || tui.needs_migration()
        || bundles.needs_migration();

    Ok(CheckReport {
        needs_migration,
        db,
        config,
        tui,
        bundles,
    })
}

/// Execute v1-to-v2 migration across all configured targets.
///
/// Before modifying any file (when `dry_run` is `false`), writes a complete
/// SHA-256 backup snapshot and `manifest.json` so [`rollback`] can restore
/// every byte-exact original.
pub async fn run_migration(targets: &MigrationTargets, options: &RunOptions) -> Result<RunReport> {
    let db_check = check_db(&targets.db).await?;
    let config_plan = plan_config_migration(&targets.config)?;
    let tui_plan = plan_tui_migration(&targets.tui_toml, &targets.tui_json)?;
    let bundles_plan = plan_bundles_migration(&targets.registry, &targets.bundles_dirs).await?;

    let mut backup_targets = Vec::new();
    if db_check.status == DbCheckStatus::NeedsMigration {
        backup_targets.push(BackupTarget {
            path: targets.db.clone(),
            kind: ArtifactKind::SqliteDb,
        });
    }
    backup_targets.extend(config_plan.backup_targets());
    backup_targets.extend(tui_plan.backup_targets());
    backup_targets.extend(bundles_plan.backup_targets());

    let (backup_dir, backup_manifest) = if !options.dry_run && !backup_targets.is_empty() {
        let dir = options
            .backup_dir
            .clone()
            .unwrap_or_else(|| default_backup_dir(&targets.config));
        let manifest = create_backup_snapshot(&dir, &backup_targets)?;
        (Some(dir), Some(manifest))
    } else {
        (None, None)
    };

    let db = migrate_db(&targets.db, options.dry_run).await?;
    let config = apply_config_plan(&config_plan, options.dry_run)?;
    let tui = apply_tui_plan(&tui_plan, options.dry_run)?;
    let bundles = apply_bundles_plan(&bundles_plan, options.dry_run).await?;

    Ok(RunReport {
        dry_run: options.dry_run,
        backup_dir,
        backup_manifest,
        db,
        config,
        tui,
        bundles,
    })
}

/// Verify every file in `backup_dir` against its SHA-256 manifest, then
/// atomically restore byte-exact originals.
pub fn rollback(backup_dir: &Path) -> Result<RollbackReport> {
    rollback_from_backup(backup_dir)
}

fn default_backup_dir(config_path: &Path) -> PathBuf {
    let base = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(default_config_dir);
    let ts = hya_proto::now_millis();
    base.join("migration-backups")
        .join(format!("v1-to-v2-{ts}"))
}

fn default_config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("hya");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".config").join("hya");
    }
    PathBuf::from(".config").join("hya")
}

fn default_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("hya");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".local").join("state").join("hya");
    }
    PathBuf::from(".local").join("state").join("hya")
}

fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("hya");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".local").join("share").join("hya");
    }
    PathBuf::from(".local").join("share").join("hya")
}
