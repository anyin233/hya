//! SHA-256 backup snapshots, atomic temp-file writes/swaps, and verified rollback.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::{MigrationError, Result};

/// Schema version of the backup manifest file (`manifest.json`).
pub const BACKUP_MANIFEST_VERSION: u32 = 1;

/// Filename of the SHA-256 manifest inside a backup directory.
pub const BACKUP_MANIFEST_FILE: &str = "manifest.json";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Category of an artifact recorded in a [`BackupManifest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// `sessions.db` SQLite database.
    SqliteDb,
    /// SQLite `-wal` or `-shm` sidecar associated with a database.
    SqliteSidecar,
    /// Top-level `config.yaml`.
    ConfigYaml,
    /// Per-bundle `<config-dir>/bundles/<id>/config.yml`.
    BundleConfig,
    /// TUI declarative structure file `tui.toml`.
    TuiToml,
    /// TUI preferences / saved `paneLayout` file `tui.json`.
    TuiJson,
    /// Installed-bundle registry `registry.sqlite3`.
    BundleRegistry,
    /// User/project bundle manifest (`bundle.yaml` or `bundle.hya.md`).
    BundleManifest,
}

/// One file entry tracked in a [`BackupManifest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    /// Canonical absolute path of the live artifact on disk.
    pub original_path: PathBuf,
    /// Relative path inside the backup directory when `existed` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_rel_path: Option<String>,
    /// Lowercase hex SHA-256 digest of the backed-up original bytes when `existed` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Byte length of the original file when `existed` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Whether `original_path` existed prior to migration (`false` => delete on rollback).
    pub existed: bool,
    /// Artifact category.
    pub kind: ArtifactKind,
}

/// Manifest written before any file is modified during `hya-migration run`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Backup manifest format version (`1`).
    pub schema_version: u32,
    /// UTC RFC-3339 timestamp when the snapshot was created.
    pub created_at: String,
    /// `hya` aggregate release version that produced this backup.
    pub tool_version: String,
    /// Ordered list of backed-up existing files and newly created targets.
    pub entries: Vec<BackupEntry>,
}

/// Summary produced by [`rollback_from_backup`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReport {
    /// Backup directory that was verified and restored.
    pub backup_dir: PathBuf,
    /// Number of backup entries whose SHA-256 matched before restoration.
    pub verified_files: usize,
    /// Original files restored byte-for-byte.
    pub restored_files: Vec<PathBuf>,
    /// Newly created files removed during rollback.
    pub removed_created_files: Vec<PathBuf>,
}

/// Compute lowercase hex SHA-256 of an in-memory byte slice.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Compute lowercase hex SHA-256 and byte length of a file on disk.
pub fn file_sha256(path: &Path) -> Result<(String, u64)> {
    let bytes =
        fs::read(path).map_err(|err| MigrationError::io(path, "read file for sha256", err))?;
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    Ok((sha256_hex(&bytes), len))
}

/// Generate a unique sibling temp file path inside `target`'s parent directory.
pub fn unique_temp_path(target: &Path, tag: &str) -> Result<PathBuf> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let filename = format!(".{name}.{tag}.{pid}.{seq}.tmp");
    Ok(parent.join(filename))
}

/// Write `bytes` atomically to `target` using a synced temp file in the same directory.
pub fn atomic_write_bytes(target: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|err| MigrationError::io(parent, "create parent directory", err))?;
    }
    let temp = unique_temp_path(target, "hya-mig")?;
    let write_res = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|err| MigrationError::io(&temp, "create atomic temp file", err))?;
        file.write_all(bytes)
            .map_err(|err| MigrationError::io(&temp, "write atomic temp file", err))?;
        file.sync_all()
            .map_err(|err| MigrationError::io(&temp, "sync atomic temp file", err))?;
        Ok(())
    })();
    if let Err(err) = write_res {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp, target) {
        let _ = fs::remove_file(&temp);
        return Err(MigrationError::io(target, "atomic rename temp file", err));
    }
    sync_parent_dir(target);
    Ok(())
}

/// Atomically swap an already-populated temp file (`temp_path`, in `target`'s parent dir)
/// onto `target`, fsyncing the file and parent directory.
pub fn atomic_swap_file(temp_path: &Path, target: &Path) -> Result<()> {
    if let Ok(file) = File::open(temp_path) {
        let _ = file.sync_all();
    }
    if let Err(err) = fs::rename(temp_path, target) {
        let _ = fs::remove_file(temp_path);
        return Err(MigrationError::io(target, "atomic swap file", err));
    }
    sync_parent_dir(target);
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

/// Return the SQLite WAL and SHM sidecar paths for a database file path.
#[must_use]
pub fn sqlite_sidecar_paths(db_path: &Path) -> [PathBuf; 2] {
    let mut wal = db_path.as_os_str().to_os_string();
    wal.push("-wal");
    let mut shm = db_path.as_os_str().to_os_string();
    shm.push("-shm");
    [PathBuf::from(wal), PathBuf::from(shm)]
}

/// Remove any `-wal` and `-shm` sidecar files next to `db_path`.
pub fn remove_sqlite_sidecars(db_path: &Path) -> Result<()> {
    for sidecar in sqlite_sidecar_paths(db_path) {
        if sidecar.exists() {
            fs::remove_file(&sidecar)
                .map_err(|err| MigrationError::io(&sidecar, "remove sqlite sidecar", err))?;
        }
    }
    Ok(())
}

/// One planned target path to record in the backup snapshot prior to mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupTarget {
    /// Path that will be modified or created.
    pub path: PathBuf,
    /// Kind of artifact.
    pub kind: ArtifactKind,
}

/// Snapshot all `targets` (plus any SQLite `-wal`/`-shm` sidecars) into `backup_dir`
/// and atomically write `manifest.json`.
///
/// Must be called BEFORE modifying any target file.
pub fn create_backup_snapshot(
    backup_dir: &Path,
    targets: &[BackupTarget],
) -> Result<BackupManifest> {
    if backup_dir.join(BACKUP_MANIFEST_FILE).exists() {
        return Err(MigrationError::BackupVerification(format!(
            "backup directory `{}` already contains `{BACKUP_MANIFEST_FILE}`; choose an empty directory",
            backup_dir.display()
        )));
    }
    let files_dir = backup_dir.join("files");
    fs::create_dir_all(&files_dir)
        .map_err(|err| MigrationError::io(&files_dir, "create backup files directory", err))?;

    let mut expanded: Vec<BackupTarget> = Vec::with_capacity(targets.len() + 4);
    let mut seen = BTreeSet::new();
    for target in targets {
        let abs = std::path::absolute(&target.path)
            .map_err(|err| MigrationError::io(&target.path, "resolve absolute path", err))?;
        if seen.insert(abs.clone()) {
            expanded.push(BackupTarget {
                path: abs.clone(),
                kind: target.kind,
            });
        }
        if matches!(
            target.kind,
            ArtifactKind::SqliteDb | ArtifactKind::BundleRegistry
        ) {
            for sidecar in sqlite_sidecar_paths(&abs) {
                if sidecar.exists() && seen.insert(sidecar.clone()) {
                    expanded.push(BackupTarget {
                        path: sidecar,
                        kind: ArtifactKind::SqliteSidecar,
                    });
                }
            }
        }
    }

    let mut entries = Vec::with_capacity(expanded.len());
    for (idx, item) in expanded.into_iter().enumerate() {
        if item.path.exists() {
            let bytes = fs::read(&item.path).map_err(|err| {
                MigrationError::io(&item.path, "read original file for backup", err)
            })?;
            let digest = sha256_hex(&bytes);
            let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let safe_leaf = item
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("artifact")
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            let rel = format!("files/{idx:04}_{safe_leaf}");
            let dst = backup_dir.join(&rel);
            atomic_write_bytes(&dst, &bytes)?;
            entries.push(BackupEntry {
                original_path: item.path,
                backup_rel_path: Some(rel),
                sha256: Some(digest),
                size_bytes: Some(size_bytes),
                existed: true,
                kind: item.kind,
            });
        } else {
            entries.push(BackupEntry {
                original_path: item.path,
                backup_rel_path: None,
                sha256: None,
                size_bytes: None,
                existed: false,
                kind: item.kind,
            });
        }
    }

    let created_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());

    let manifest = BackupManifest {
        schema_version: BACKUP_MANIFEST_VERSION,
        created_at,
        tool_version: hya_version::VERSION.to_owned(),
        entries,
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|err| MigrationError::Json {
            context: "serialize backup manifest".to_owned(),
            source: err,
        })?;
    atomic_write_bytes(&backup_dir.join(BACKUP_MANIFEST_FILE), &manifest_bytes)?;
    Ok(manifest)
}

/// Read and validate `manifest.json` and verify every backed-up file's SHA-256 digest.
///
/// Never modifies any target file.
pub fn verify_backup_manifest(backup_dir: &Path) -> Result<BackupManifest> {
    let manifest_path = backup_dir.join(BACKUP_MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(MigrationError::BackupVerification(format!(
            "backup manifest not found at `{}`",
            manifest_path.display()
        )));
    }
    let raw = fs::read(&manifest_path)
        .map_err(|err| MigrationError::io(&manifest_path, "read backup manifest", err))?;
    let manifest: BackupManifest =
        serde_json::from_slice(&raw).map_err(|err| MigrationError::Json {
            context: format!("parse backup manifest {}", manifest_path.display()),
            source: err,
        })?;
    if manifest.schema_version != BACKUP_MANIFEST_VERSION {
        return Err(MigrationError::BackupVerification(format!(
            "unsupported backup manifest schema_version `{}` (expected `{BACKUP_MANIFEST_VERSION}`)",
            manifest.schema_version
        )));
    }

    for entry in &manifest.entries {
        if !entry.existed {
            continue;
        }
        let rel = entry.backup_rel_path.as_deref().ok_or_else(|| {
            MigrationError::BackupVerification(format!(
                "manifest entry for `{}` is missing `backup_rel_path`",
                entry.original_path.display()
            ))
        })?;
        let expected_sha = entry.sha256.as_deref().ok_or_else(|| {
            MigrationError::BackupVerification(format!(
                "manifest entry for `{}` is missing `sha256`",
                entry.original_path.display()
            ))
        })?;
        let expected_size = entry.size_bytes.ok_or_else(|| {
            MigrationError::BackupVerification(format!(
                "manifest entry for `{}` is missing `size_bytes`",
                entry.original_path.display()
            ))
        })?;
        let backup_file = backup_dir.join(rel);
        if !backup_file.is_file() {
            return Err(MigrationError::BackupVerification(format!(
                "backed-up file `{}` is missing for `{}`",
                backup_file.display(),
                entry.original_path.display()
            )));
        }
        let (actual_sha, actual_size) = file_sha256(&backup_file)?;
        if actual_size != expected_size {
            return Err(MigrationError::BackupVerification(format!(
                "size mismatch for `{}`: expected {expected_size} bytes, found {actual_size} bytes",
                backup_file.display()
            )));
        }
        if actual_sha != expected_sha {
            return Err(MigrationError::BackupVerification(format!(
                "SHA-256 mismatch for `{}` (target `{}`): expected {expected_sha}, found {actual_sha}",
                backup_file.display(),
                entry.original_path.display()
            )));
        }
    }

    Ok(manifest)
}

/// Verify every file in `backup_dir` against `manifest.json`, then atomically restore
/// all byte-exact originals and remove files created by migration.
pub fn rollback_from_backup(backup_dir: &Path) -> Result<RollbackReport> {
    let manifest = verify_backup_manifest(backup_dir)?;

    let mut verified_files = 0usize;
    let mut restored_files = Vec::new();
    let mut removed_created_files = Vec::new();

    // First, track which SQLite sidecars exist in the backup so we only clear unbacked sidecars.
    let backed_up_paths: BTreeSet<PathBuf> = manifest
        .entries
        .iter()
        .filter(|e| e.existed)
        .map(|e| e.original_path.clone())
        .collect();

    for entry in &manifest.entries {
        if entry.existed {
            verified_files += 1;
            let rel = entry.backup_rel_path.as_deref().ok_or_else(|| {
                MigrationError::BackupVerification(
                    "missing backup_rel_path after verify".to_owned(),
                )
            })?;
            let backup_file = backup_dir.join(rel);
            let bytes = fs::read(&backup_file).map_err(|err| {
                MigrationError::io(&backup_file, "read verified backup file", err)
            })?;

            if matches!(
                entry.kind,
                ArtifactKind::SqliteDb | ArtifactKind::BundleRegistry
            ) {
                for sidecar in sqlite_sidecar_paths(&entry.original_path) {
                    if !backed_up_paths.contains(&sidecar) && sidecar.exists() {
                        fs::remove_file(&sidecar).map_err(|err| {
                            MigrationError::io(
                                &sidecar,
                                "remove post-migration sqlite sidecar",
                                err,
                            )
                        })?;
                    }
                }
            }

            atomic_write_bytes(&entry.original_path, &bytes)?;
            restored_files.push(entry.original_path.clone());
        } else if entry.original_path.exists() {
            fs::remove_file(&entry.original_path).map_err(|err| {
                MigrationError::io(
                    &entry.original_path,
                    "remove file created during migration",
                    err,
                )
            })?;
            removed_created_files.push(entry.original_path.clone());
            // Clean up empty bundle config directories created during migration.
            if entry.kind == ArtifactKind::BundleConfig
                && let Some(bundle_dir) = entry.original_path.parent()
                && fs::read_dir(bundle_dir)
                    .ok()
                    .is_some_and(|mut it| it.next().is_none())
            {
                let _ = fs::remove_dir(bundle_dir);
                if let Some(bundles_root) = bundle_dir.parent()
                    && bundles_root
                        .file_name()
                        .is_some_and(|name| name == "bundles")
                    && fs::read_dir(bundles_root)
                        .ok()
                        .is_some_and(|mut it| it.next().is_none())
                {
                    let _ = fs::remove_dir(bundles_root);
                }
            }
        }
    }

    Ok(RollbackReport {
        backup_dir: backup_dir.to_path_buf(),
        verified_files,
        restored_files,
        removed_created_files,
    })
}
