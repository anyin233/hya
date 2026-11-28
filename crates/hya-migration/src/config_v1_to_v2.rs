//! `config.yaml` v1-to-v2 migration.
//!
//! Extracts v1 domain blocks (`websearch`, `webfetch`, `lsp`, `formatter`,
//! `subagents`, `goal`, `compaction`) into
//! `<config-dir>/bundles/<percent-encoded-id>/config.yml`, remaps legacy
//! first-party bundle IDs in `bundles.builtin.{enable,disable}`, and writes a
//! clean `schema_version: 2` `config.yaml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_norway::{Mapping, Value};

use crate::backup::{ArtifactKind, BackupTarget, atomic_write_bytes};
use crate::bundle_ids::{remap_bundle_id_list, user_bundle_config_path};
use crate::error::{MigrationError, Result};

/// Target `config.yaml` schema version.
pub const CONFIG_SCHEMA_VERSION_V2: u64 = 2;

/// Status of `config.yaml` returned by [`check_config`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigCheckStatus {
    /// `config.yaml` does not exist.
    NotFound,
    /// `config.yaml` is already valid v2 with no legacy sections or bundle IDs.
    AlreadyV2,
    /// `config.yaml` requires migration to v2.
    NeedsMigration,
}

/// Read-only inspection report for `config.yaml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigCheckReport {
    /// Path to `config.yaml`.
    pub path: PathBuf,
    /// Inspection classification.
    pub status: ConfigCheckStatus,
    /// Existing `schema_version`, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
    /// Legacy domain sections found in `config.yaml` that will be extracted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extracted_sections: Vec<String>,
    /// Target per-bundle `config.yml` files that will be created or updated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_bundle_configs: Vec<PathBuf>,
    /// Legacy bundle IDs found under `bundles.builtin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remapped_bundle_ids: Vec<String>,
    /// Human-readable reasons why migration is needed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// Result summary for migrating `config.yaml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigMigrationReport {
    /// Path to `config.yaml`.
    pub path: PathBuf,
    /// Whether files were written (`false` on dry-run or already-v2).
    pub migrated: bool,
    /// Per-bundle `config.yml` files created or updated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundle_configs_written: Vec<PathBuf>,
    /// Diagnostics and remapping notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Planned config file mutations so the orchestrator can create a complete
/// SHA-256 backup snapshot BEFORE writing any file.
#[derive(Clone, Debug)]
pub struct ConfigMigrationPlan {
    /// Whether any change is needed.
    pub needs_migration: bool,
    /// Path to `config.yaml`.
    pub config_path: PathBuf,
    /// New serialized `config.yaml` bytes (when `needs_migration` is true).
    pub new_config_yaml: Option<String>,
    /// Map of `<bundle-config-path> -> new YAML content`.
    pub bundle_config_writes: BTreeMap<PathBuf, String>,
    /// Diagnostics and remapping notes.
    pub notes: Vec<String>,
}

impl ConfigMigrationPlan {
    /// Return every file that will be modified or created by this plan.
    #[must_use]
    pub fn backup_targets(&self) -> Vec<BackupTarget> {
        if !self.needs_migration {
            return Vec::new();
        }
        let mut out = vec![BackupTarget {
            path: self.config_path.clone(),
            kind: ArtifactKind::ConfigYaml,
        }];
        for path in self.bundle_config_writes.keys() {
            out.push(BackupTarget {
                path: path.clone(),
                kind: ArtifactKind::BundleConfig,
            });
        }
        out
    }
}

/// Top-level v1 sections mapped to `(target_bundle_id, section_key_in_bundle_config)`.
const TOP_LEVEL_DOMAIN_SECTIONS: [(&str, &str, &str); 7] = [
    ("compaction", "hya/compaction", "compaction"),
    ("goal", "hya/goal", "goal"),
    ("subagents", "hya/multi-agent", "subagents"),
    ("formatter", "hya/coding-extras", "formatter"),
    ("lsp", "hya/coding-extras", "lsp"),
    ("websearch", "hya/network", "websearch"),
    ("webfetch", "hya/network", "webfetch"),
];

/// Inspect `config.yaml` without writing anything.
pub fn check_config(config_path: &Path) -> Result<ConfigCheckReport> {
    if !config_path.is_file() {
        return Ok(ConfigCheckReport {
            path: config_path.to_path_buf(),
            status: ConfigCheckStatus::NotFound,
            schema_version: None,
            extracted_sections: Vec::new(),
            target_bundle_configs: Vec::new(),
            remapped_bundle_ids: Vec::new(),
            reasons: Vec::new(),
        });
    }

    let raw = fs::read_to_string(config_path)
        .map_err(|err| MigrationError::io(config_path, "read config.yaml", err))?;
    let root = parse_yaml_mapping(config_path, &raw)?;
    let schema_version = root
        .get(Value::String("schema_version".to_owned()))
        .and_then(Value::as_u64);

    let mut reasons = Vec::new();
    let mut extracted_sections = Vec::new();
    let mut target_bundle_configs = Vec::new();
    let mut add_target = |bundle_id: &str| {
        let p = user_bundle_config_path(config_path, bundle_id);
        if !target_bundle_configs.contains(&p) {
            target_bundle_configs.push(p);
        }
    };

    if schema_version != Some(CONFIG_SCHEMA_VERSION_V2) {
        let found = schema_version.map_or_else(|| "missing".to_owned(), |v| v.to_string());
        reasons.push(format!(
            "config.yaml `schema_version` is `{found}` (v2 requires `2`)"
        ));
    }

    for (key, bundle_id, _) in TOP_LEVEL_DOMAIN_SECTIONS {
        if root.contains_key(Value::String(key.to_owned())) {
            extracted_sections.push(key.to_owned());
            add_target(bundle_id);
            reasons.push(format!(
                "top-level section `{key}` moves to `{}`",
                user_bundle_config_path(config_path, bundle_id).display()
            ));
        }
    }

    if let Some(tools) = root
        .get(Value::String("tools".to_owned()))
        .and_then(Value::as_mapping)
    {
        for key in ["websearch", "webfetch"] {
            if tools.contains_key(Value::String(key.to_owned())) {
                extracted_sections.push(format!("tools.{key}"));
                add_target("hya/network");
                reasons.push(format!(
                    "section `tools.{key}` moves to `{}`",
                    user_bundle_config_path(config_path, "hya/network").display()
                ));
            }
        }
    }

    let mut remapped_bundle_ids = Vec::new();
    if let Some(builtin) = root
        .get(Value::String("bundles".to_owned()))
        .and_then(Value::as_mapping)
        .and_then(|b| b.get(Value::String("builtin".to_owned())))
        .and_then(Value::as_mapping)
    {
        for field in ["enable", "disable"] {
            if let Some(seq) = builtin
                .get(Value::String(field.to_owned()))
                .and_then(Value::as_sequence)
            {
                let ids: Vec<String> = seq
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let (_, changed, notes) = remap_bundle_id_list(&ids);
                if changed {
                    for note in notes {
                        reasons.push(format!("bundles.builtin.{field}: {note}"));
                        remapped_bundle_ids.push(note);
                    }
                }
            }
        }
    }

    let status = if reasons.is_empty() {
        ConfigCheckStatus::AlreadyV2
    } else {
        ConfigCheckStatus::NeedsMigration
    };

    Ok(ConfigCheckReport {
        path: config_path.to_path_buf(),
        status,
        schema_version,
        extracted_sections,
        target_bundle_configs,
        remapped_bundle_ids,
        reasons,
    })
}

/// Build the full in-memory migration plan for `config.yaml` without writing to disk.
pub fn plan_config_migration(config_path: &Path) -> Result<ConfigMigrationPlan> {
    let check = check_config(config_path)?;
    if check.status != ConfigCheckStatus::NeedsMigration {
        let notes = match check.status {
            ConfigCheckStatus::NotFound => vec!["config.yaml not found; skipped".to_owned()],
            ConfigCheckStatus::AlreadyV2 => {
                vec!["config.yaml is already v2; no changes needed".to_owned()]
            }
            ConfigCheckStatus::NeedsMigration => Vec::new(),
        };
        return Ok(ConfigMigrationPlan {
            needs_migration: false,
            config_path: config_path.to_path_buf(),
            new_config_yaml: None,
            bundle_config_writes: BTreeMap::new(),
            notes,
        });
    }

    let raw = fs::read_to_string(config_path)
        .map_err(|err| MigrationError::io(config_path, "read config.yaml", err))?;
    let mut root = parse_yaml_mapping(config_path, &raw)?;
    let mut notes = Vec::new();

    // Bundle id -> mapping of extracted sections (`section_key -> Value`).
    let mut extracted_by_bundle: BTreeMap<&'static str, Mapping> = BTreeMap::new();

    for (top_key, bundle_id, section_key) in TOP_LEVEL_DOMAIN_SECTIONS {
        if let Some(val) = root.remove(Value::String(top_key.to_owned())) {
            extracted_by_bundle
                .entry(bundle_id)
                .or_default()
                .insert(Value::String(section_key.to_owned()), val);
            notes.push(format!(
                "extracted `{top_key}` -> bundle `{bundle_id}` (`{section_key}`)"
            ));
        }
    }

    // Extract `tools.websearch` and `tools.webfetch`; drop `tools` if it becomes empty.
    let mut remove_tools_key = false;
    if let Some(tools_val) = root.get_mut(Value::String("tools".to_owned()))
        && let Some(tools_map) = tools_val.as_mapping_mut()
    {
        for key in ["websearch", "webfetch"] {
            if let Some(val) = tools_map.remove(Value::String(key.to_owned())) {
                extracted_by_bundle
                    .entry("hya/network")
                    .or_default()
                    .insert(Value::String(key.to_owned()), val);
                notes.push(format!(
                    "extracted `tools.{key}` -> bundle `hya/network` (`{key}`)"
                ));
            }
        }
        if tools_map.is_empty() {
            remove_tools_key = true;
        }
    }
    if remove_tools_key {
        root.remove(Value::String("tools".to_owned()));
    }

    // Remap legacy bundle IDs in `bundles.builtin.{enable,disable}`, or ensure a
    // default `bundles.builtin` block exists so v2 config is self-documenting.
    remap_bundles_section(&mut root, &mut notes);

    // Emit `schema_version: 2` first, preserving remaining top-level key order.
    root.remove(Value::String("schema_version".to_owned()));
    let mut ordered = Mapping::with_capacity(root.len() + 1);
    ordered.insert(
        Value::String("schema_version".to_owned()),
        Value::Number(CONFIG_SCHEMA_VERSION_V2.into()),
    );
    for (k, v) in root {
        ordered.insert(k, v);
    }

    let new_config_yaml =
        serde_norway::to_string(&Value::Mapping(ordered)).map_err(|err| MigrationError::Yaml {
            context: format!("serialize migrated {}", config_path.display()),
            source: err,
        })?;

    // Merge extracted domain blocks into each target `<config-dir>/bundles/<encoded>/config.yml`.
    let mut bundle_config_writes = BTreeMap::new();
    for (bundle_id, sections) in extracted_by_bundle {
        let target_path = user_bundle_config_path(config_path, bundle_id);
        let mut merged = if target_path.is_file() {
            let existing = fs::read_to_string(&target_path).map_err(|err| {
                MigrationError::io(&target_path, "read existing bundle config.yml", err)
            })?;
            if existing.trim().is_empty() {
                Mapping::new()
            } else {
                parse_yaml_mapping(&target_path, &existing)?
            }
        } else {
            Mapping::new()
        };
        for (k, v) in sections {
            merged.insert(k, v);
        }
        let serialized = serde_norway::to_string(&Value::Mapping(merged)).map_err(|err| {
            MigrationError::Yaml {
                context: format!("serialize bundle config {}", target_path.display()),
                source: err,
            }
        })?;
        bundle_config_writes.insert(target_path, serialized);
    }

    Ok(ConfigMigrationPlan {
        needs_migration: true,
        config_path: config_path.to_path_buf(),
        new_config_yaml: Some(new_config_yaml),
        bundle_config_writes,
        notes,
    })
}

/// Apply a precomputed [`ConfigMigrationPlan`] using atomic temp-file writes.
pub fn apply_config_plan(
    plan: &ConfigMigrationPlan,
    dry_run: bool,
) -> Result<ConfigMigrationReport> {
    if !plan.needs_migration {
        return Ok(ConfigMigrationReport {
            path: plan.config_path.clone(),
            migrated: false,
            bundle_configs_written: Vec::new(),
            notes: plan.notes.clone(),
        });
    }
    if dry_run {
        let mut notes = plan.notes.clone();
        notes.push(format!(
            "dry-run: would write `{}` and {} bundle config file(s)",
            plan.config_path.display(),
            plan.bundle_config_writes.len()
        ));
        return Ok(ConfigMigrationReport {
            path: plan.config_path.clone(),
            migrated: false,
            bundle_configs_written: plan.bundle_config_writes.keys().cloned().collect(),
            notes,
        });
    }

    let mut written = Vec::with_capacity(plan.bundle_config_writes.len());
    for (path, yaml) in &plan.bundle_config_writes {
        atomic_write_bytes(path, yaml.as_bytes())?;
        written.push(path.clone());
    }
    if let Some(yaml) = &plan.new_config_yaml {
        atomic_write_bytes(&plan.config_path, yaml.as_bytes())?;
    }

    Ok(ConfigMigrationReport {
        path: plan.config_path.clone(),
        migrated: true,
        bundle_configs_written: written,
        notes: plan.notes.clone(),
    })
}

fn remap_bundles_section(root: &mut Mapping, notes: &mut Vec<String>) {
    let bundles_key = Value::String("bundles".to_owned());
    let builtin_key = Value::String("builtin".to_owned());

    if !root.contains_key(&bundles_key) {
        let mut builtin = Mapping::new();
        builtin.insert(
            Value::String("profile".to_owned()),
            Value::String("full".to_owned()),
        );
        builtin.insert(
            Value::String("enable".to_owned()),
            Value::Sequence(Vec::new()),
        );
        builtin.insert(
            Value::String("disable".to_owned()),
            Value::Sequence(Vec::new()),
        );
        let mut bundles = Mapping::new();
        bundles.insert(builtin_key, Value::Mapping(builtin));
        root.insert(bundles_key, Value::Mapping(bundles));
        notes.push("added default `bundles.builtin` (`profile: full`)".to_owned());
        return;
    }

    if let Some(bundles_map) = root.get_mut(&bundles_key).and_then(Value::as_mapping_mut)
        && let Some(builtin_map) = bundles_map
            .get_mut(&builtin_key)
            .and_then(Value::as_mapping_mut)
    {
        for field in ["enable", "disable"] {
            let field_key = Value::String(field.to_owned());
            if let Some(seq) = builtin_map.get(&field_key).and_then(Value::as_sequence) {
                let ids: Vec<String> = seq
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let (remapped, changed, remap_notes) = remap_bundle_id_list(&ids);
                if changed {
                    builtin_map.insert(
                        field_key,
                        Value::Sequence(remapped.into_iter().map(Value::String).collect()),
                    );
                    for note in remap_notes {
                        notes.push(format!("bundles.builtin.{field}: {note}"));
                    }
                }
            }
        }
    }
}

fn parse_yaml_mapping(path: &Path, raw: &str) -> Result<Mapping> {
    if raw.trim().is_empty() {
        return Ok(Mapping::new());
    }
    let val: Value = serde_norway::from_str(raw).map_err(|err| MigrationError::Yaml {
        context: format!("parse {}", path.display()),
        source: err,
    })?;
    match val {
        Value::Mapping(map) => Ok(map),
        Value::Null => Ok(Mapping::new()),
        _ => Err(MigrationError::InvalidArtifact {
            path: path.to_path_buf(),
            detail: "expected a top-level YAML mapping".to_owned(),
        }),
    }
}
