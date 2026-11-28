//! TUI `tui.toml` and `tui.json` (`paneLayout`) v1-to-v2 migration.
//!
//! Upgrades:
//! - `tui.toml`: bumps `version = 1` -> `version = 2`, migrates hardcoded
//!   `[subagents]` / `[workflows]` sections (at top level and inside
//!   `[presets.<name>]`) to dynamic bundle pane tables
//!   `[panes."hya/multi-agent#subagents"]` / `[panes."hya/workflow#workflows"]`,
//!   and rewrites legacy pane kind strings inside `[layout]`.
//! - `tui.json`: upgrades legacy binary `paneLayout` (versions 1/2/3 from
//!   `legacyPaneLayout.ts`) into v4 n-ary split trees, converts hardcoded domain
//!   pane kinds (`subagents`, `subagent-viewer`, `workflows`) to v2 dynamic
//!   extension panes (`kind: "extension"`, `panel: "<bundle>#<panel>"`), and
//!   remaps legacy bundle ID keys in `extensionEnabled` / `extensionTrusted`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue, json};
use toml::Table as TomlTable;
use toml::Value as TomlValue;

use crate::backup::{ArtifactKind, BackupTarget, atomic_write_bytes};
use crate::bundle_ids::{BundleIdRemap, remap_bundle_id};
use crate::error::{MigrationError, Result};

/// Target `tui.toml` schema version.
pub const TUI_TOML_VERSION_V2: i64 = 2;

/// Target `paneLayout` structural version (n-ary split tree with dynamic extension panes).
pub const PANE_LAYOUT_VERSION_V4: u64 = 4;

/// Dynamic v2 extension panel key for the legacy `subagents` pane.
pub const PANEL_SUBAGENTS: &str = "hya/multi-agent#subagents";
/// Dynamic v2 extension panel key for the legacy `subagent-viewer` pane.
pub const PANEL_SUBAGENT_VIEWER: &str = "hya/multi-agent#subagent-viewer";
/// Dynamic v2 extension panel key for the legacy `workflows` pane.
pub const PANEL_WORKFLOWS: &str = "hya/workflow#workflows";

/// Status of one TUI configuration artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiCheckStatus {
    /// File does not exist.
    NotFound,
    /// Already in v2 format.
    AlreadyV2,
    /// Requires v1-to-v2 migration.
    NeedsMigration,
}

/// Read-only inspection report for `tui.toml` and `tui.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiCheckReport {
    /// Path to `tui.toml`.
    pub tui_toml_path: PathBuf,
    /// Status of `tui.toml`.
    pub tui_toml_status: TuiCheckStatus,
    /// Reasons `tui.toml` needs migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tui_toml_reasons: Vec<String>,
    /// Path to `tui.json`.
    pub tui_json_path: PathBuf,
    /// Status of `tui.json`.
    pub tui_json_status: TuiCheckStatus,
    /// Reasons `tui.json` needs migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tui_json_reasons: Vec<String>,
}

impl TuiCheckReport {
    /// Whether either `tui.toml` or `tui.json` requires migration.
    #[must_use]
    pub fn needs_migration(&self) -> bool {
        self.tui_toml_status == TuiCheckStatus::NeedsMigration
            || self.tui_json_status == TuiCheckStatus::NeedsMigration
    }
}

/// Result summary for migrating `tui.toml` and `tui.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TuiMigrationReport {
    /// Whether `tui.toml` was written.
    pub tui_toml_migrated: bool,
    /// Whether `tui.json` was written.
    pub tui_json_migrated: bool,
    /// Diagnostics and conversion notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Planned TUI file mutations so backups can be captured before writing.
#[derive(Clone, Debug)]
pub struct TuiMigrationPlan {
    /// Path to `tui.toml`.
    pub tui_toml_path: PathBuf,
    /// New `tui.toml` contents when migration is needed.
    pub new_tui_toml: Option<String>,
    /// Path to `tui.json`.
    pub tui_json_path: PathBuf,
    /// New `tui.json` contents when migration is needed.
    pub new_tui_json: Option<String>,
    /// Diagnostics and conversion notes.
    pub notes: Vec<String>,
}

impl TuiMigrationPlan {
    /// Whether either file will be updated.
    #[must_use]
    pub fn needs_migration(&self) -> bool {
        self.new_tui_toml.is_some() || self.new_tui_json.is_some()
    }

    /// Files that will be modified by this plan.
    #[must_use]
    pub fn backup_targets(&self) -> Vec<BackupTarget> {
        let mut out = Vec::new();
        if self.new_tui_toml.is_some() {
            out.push(BackupTarget {
                path: self.tui_toml_path.clone(),
                kind: ArtifactKind::TuiToml,
            });
        }
        if self.new_tui_json.is_some() {
            out.push(BackupTarget {
                path: self.tui_json_path.clone(),
                kind: ArtifactKind::TuiJson,
            });
        }
        out
    }
}

/// Map a legacy hardcoded domain pane kind to its v2 dynamic bundle panel key.
#[must_use]
pub fn legacy_pane_kind_to_panel(kind: &str) -> Option<&'static str> {
    match kind {
        "subagents" => Some(PANEL_SUBAGENTS),
        "subagent-viewer" => Some(PANEL_SUBAGENT_VIEWER),
        "workflows" => Some(PANEL_WORKFLOWS),
        _ => None,
    }
}

/// Inspect `tui.toml` and `tui.json` without writing any file.
pub fn check_tui(tui_toml_path: &Path, tui_json_path: &Path) -> Result<TuiCheckReport> {
    let (tui_toml_status, tui_toml_reasons) = inspect_tui_toml(tui_toml_path)?;
    let (tui_json_status, tui_json_reasons) = inspect_tui_json(tui_json_path)?;
    Ok(TuiCheckReport {
        tui_toml_path: tui_toml_path.to_path_buf(),
        tui_toml_status,
        tui_toml_reasons,
        tui_json_path: tui_json_path.to_path_buf(),
        tui_json_status,
        tui_json_reasons,
    })
}

/// Plan `tui.toml` and `tui.json` migrations in memory without writing to disk.
pub fn plan_tui_migration(tui_toml_path: &Path, tui_json_path: &Path) -> Result<TuiMigrationPlan> {
    let mut notes = Vec::new();
    let new_tui_toml = build_migrated_tui_toml(tui_toml_path, &mut notes)?;
    let new_tui_json = build_migrated_tui_json(tui_json_path, &mut notes)?;
    Ok(TuiMigrationPlan {
        tui_toml_path: tui_toml_path.to_path_buf(),
        new_tui_toml,
        tui_json_path: tui_json_path.to_path_buf(),
        new_tui_json,
        notes,
    })
}

/// Apply a precomputed [`TuiMigrationPlan`] atomically.
pub fn apply_tui_plan(plan: &TuiMigrationPlan, dry_run: bool) -> Result<TuiMigrationReport> {
    if !plan.needs_migration() {
        return Ok(TuiMigrationReport {
            tui_toml_migrated: false,
            tui_json_migrated: false,
            notes: plan.notes.clone(),
        });
    }
    if dry_run {
        let mut notes = plan.notes.clone();
        if plan.new_tui_toml.is_some() {
            notes.push(format!(
                "dry-run: would update `{}`",
                plan.tui_toml_path.display()
            ));
        }
        if plan.new_tui_json.is_some() {
            notes.push(format!(
                "dry-run: would update `{}`",
                plan.tui_json_path.display()
            ));
        }
        return Ok(TuiMigrationReport {
            tui_toml_migrated: false,
            tui_json_migrated: false,
            notes,
        });
    }

    let mut tui_toml_migrated = false;
    let mut tui_json_migrated = false;
    if let Some(content) = &plan.new_tui_toml {
        atomic_write_bytes(&plan.tui_toml_path, content.as_bytes())?;
        tui_toml_migrated = true;
    }
    if let Some(content) = &plan.new_tui_json {
        atomic_write_bytes(&plan.tui_json_path, content.as_bytes())?;
        tui_json_migrated = true;
    }
    Ok(TuiMigrationReport {
        tui_toml_migrated,
        tui_json_migrated,
        notes: plan.notes.clone(),
    })
}

// ---------------------------------------------------------------------------
// tui.toml inspection & migration
// ---------------------------------------------------------------------------

fn inspect_tui_toml(path: &Path) -> Result<(TuiCheckStatus, Vec<String>)> {
    if !path.is_file() {
        return Ok((TuiCheckStatus::NotFound, Vec::new()));
    }
    let raw =
        fs::read_to_string(path).map_err(|err| MigrationError::io(path, "read tui.toml", err))?;
    let mut table: TomlTable = toml::from_str(&raw).map_err(|source| MigrationError::TomlDe {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reasons = Vec::new();
    let _ = transform_tui_toml_table(&mut table, &mut reasons);
    let status = if reasons.is_empty() {
        TuiCheckStatus::AlreadyV2
    } else {
        TuiCheckStatus::NeedsMigration
    };
    Ok((status, reasons))
}

fn build_migrated_tui_toml(path: &Path, notes: &mut Vec<String>) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).map_err(|err| MigrationError::io(path, "read tui.toml", err))?;
    let mut table: TomlTable = toml::from_str(&raw).map_err(|source| MigrationError::TomlDe {
        path: path.to_path_buf(),
        source,
    })?;
    let mut changes = Vec::new();
    if !transform_tui_toml_table(&mut table, &mut changes) {
        return Ok(None);
    }
    for change in changes {
        notes.push(format!("tui.toml: {change}"));
    }
    let serialized = toml::to_string_pretty(&table).map_err(|source| MigrationError::TomlSer {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(serialized))
}

fn transform_tui_toml_table(table: &mut TomlTable, changes: &mut Vec<String>) -> bool {
    let mut changed = false;

    let version = table.get("version").and_then(TomlValue::as_integer);
    if version != Some(TUI_TOML_VERSION_V2) {
        let found = version.map_or_else(|| "missing".to_owned(), |v| v.to_string());
        table.insert(
            "version".to_owned(),
            TomlValue::Integer(TUI_TOML_VERSION_V2),
        );
        changes.push(format!(
            "upgraded `version` from `{found}` to `{TUI_TOML_VERSION_V2}`"
        ));
        changed = true;
    }

    if migrate_toml_scope_panes(table, "", changes) {
        changed = true;
    }

    if let Some(TomlValue::Table(presets)) = table.get_mut("presets") {
        let preset_names: Vec<String> = presets.keys().cloned().collect();
        for name in preset_names {
            if let Some(TomlValue::Table(preset_tbl)) = presets.get_mut(&name) {
                let prefix = format!("presets.{name}.");
                if migrate_toml_scope_panes(preset_tbl, &prefix, changes) {
                    changed = true;
                }
            }
        }
    }

    changed
}

fn migrate_toml_scope_panes(
    scope: &mut TomlTable,
    prefix: &str,
    changes: &mut Vec<String>,
) -> bool {
    let mut changed = false;

    for (legacy_section, panel_key) in [
        ("subagents", PANEL_SUBAGENTS),
        ("workflows", PANEL_WORKFLOWS),
    ] {
        if let Some(val) = scope.remove(legacy_section) {
            let panes_entry = scope
                .entry("panes".to_owned())
                .or_insert_with(|| TomlValue::Table(TomlTable::new()));
            if let TomlValue::Table(panes_tbl) = panes_entry {
                panes_tbl.insert(panel_key.to_owned(), val);
            }
            changes.push(format!(
                "moved `{prefix}{legacy_section}` -> `{prefix}panes.\"{panel_key}\"`"
            ));
            changed = true;
        }
    }

    if let Some(layout_val) = scope.get_mut("layout")
        && rewrite_toml_layout_pane_kinds(layout_val, &format!("{prefix}layout"), changes)
    {
        changed = true;
    }

    changed
}

fn rewrite_toml_layout_pane_kinds(
    value: &mut TomlValue,
    path: &str,
    changes: &mut Vec<String>,
) -> bool {
    let mut changed = false;
    match value {
        TomlValue::String(s) => {
            if let Some(panel) = legacy_pane_kind_to_panel(s) {
                changes.push(format!("rewrote `{path}` pane kind `{s}` -> `{panel}`"));
                *s = panel.to_owned();
                changed = true;
            }
        }
        TomlValue::Array(items) => {
            for (idx, item) in items.iter_mut().enumerate() {
                if rewrite_toml_layout_pane_kinds(item, &format!("{path}[{idx}]"), changes) {
                    changed = true;
                }
            }
        }
        TomlValue::Table(tbl) => {
            let keys: Vec<String> = tbl.keys().cloned().collect();
            for key in keys {
                if let Some(child) = tbl.get_mut(&key)
                    && rewrite_toml_layout_pane_kinds(child, &format!("{path}.{key}"), changes)
                {
                    changed = true;
                }
            }
        }
        _ => {}
    }
    changed
}

// ---------------------------------------------------------------------------
// tui.json (`paneLayout`, `extensionEnabled`, `extensionTrusted`)
// ---------------------------------------------------------------------------

fn inspect_tui_json(path: &Path) -> Result<(TuiCheckStatus, Vec<String>)> {
    if !path.is_file() {
        return Ok((TuiCheckStatus::NotFound, Vec::new()));
    }
    let raw =
        fs::read_to_string(path).map_err(|err| MigrationError::io(path, "read tui.json", err))?;
    let mut val: JsonValue = serde_json::from_str(&raw).map_err(|source| MigrationError::Json {
        context: format!("parse {}", path.display()),
        source,
    })?;
    let Some(obj) = val.as_object_mut() else {
        return Err(MigrationError::InvalidArtifact {
            path: path.to_path_buf(),
            detail: "tui.json must be a JSON object".to_owned(),
        });
    };
    let mut reasons = Vec::new();
    let _ = transform_tui_json_object(obj, path, &mut reasons)?;
    let status = if reasons.is_empty() {
        TuiCheckStatus::AlreadyV2
    } else {
        TuiCheckStatus::NeedsMigration
    };
    Ok((status, reasons))
}

fn build_migrated_tui_json(path: &Path, notes: &mut Vec<String>) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).map_err(|err| MigrationError::io(path, "read tui.json", err))?;
    let mut val: JsonValue = serde_json::from_str(&raw).map_err(|source| MigrationError::Json {
        context: format!("parse {}", path.display()),
        source,
    })?;
    let Some(obj) = val.as_object_mut() else {
        return Err(MigrationError::InvalidArtifact {
            path: path.to_path_buf(),
            detail: "tui.json must be a JSON object".to_owned(),
        });
    };
    let mut changes = Vec::new();
    if !transform_tui_json_object(obj, path, &mut changes)? {
        return Ok(None);
    }
    for change in changes {
        notes.push(format!("tui.json: {change}"));
    }
    let mut serialized =
        serde_json::to_string_pretty(&val).map_err(|source| MigrationError::Json {
            context: format!("serialize {}", path.display()),
            source,
        })?;
    serialized.push('\n');
    Ok(Some(serialized))
}

fn transform_tui_json_object(
    obj: &mut JsonMap<String, JsonValue>,
    path: &Path,
    changes: &mut Vec<String>,
) -> Result<bool> {
    let mut changed = false;

    if let Some(layout_val) = obj.get_mut("paneLayout")
        && migrate_pane_layout_value(layout_val, path, changes)?
    {
        changed = true;
    }

    for map_key in ["extensionEnabled", "extensionTrusted"] {
        if let Some(JsonValue::Object(map)) = obj.get_mut(map_key)
            && remap_extension_flag_map(map, map_key, changes)
        {
            changed = true;
        }
    }

    Ok(changed)
}

fn remap_extension_flag_map(
    map: &mut JsonMap<String, JsonValue>,
    field_name: &str,
    changes: &mut Vec<String>,
) -> bool {
    let entries: Vec<(String, JsonValue)> =
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut next = JsonMap::with_capacity(entries.len());
    let mut changed = false;
    for (key, value) in entries {
        match remap_bundle_id(&key) {
            BundleIdRemap::Remapped { from, to } => {
                changed = true;
                changes.push(format!(
                    "{field_name}: remapped `{from}` -> [{}]",
                    to.join(", ")
                ));
                for target in to {
                    next.entry(target.to_owned())
                        .or_insert_with(|| value.clone());
                }
            }
            BundleIdRemap::PromotedToCore { from, note } => {
                changed = true;
                changes.push(format!("{field_name}: removed `{from}` ({note})"));
            }
            BundleIdRemap::Unchanged(id) => {
                next.insert(id, value);
            }
        }
    }
    if changed {
        *map = next;
    }
    changed
}

/// Upgrade a saved `paneLayout` JSON value from v1/v2/v3 binary layout or v4
/// layout with legacy domain pane kinds to v2-compatible v4 layout using
/// dynamic `kind: "extension"` + `panel: "<bundle>#<panel>"` leaves.
pub fn migrate_pane_layout_value(
    layout: &mut JsonValue,
    path: &Path,
    changes: &mut Vec<String>,
) -> Result<bool> {
    let Some(record) = layout.as_object() else {
        return Ok(false);
    };
    let version = record.get("version").and_then(JsonValue::as_u64);
    let mut changed = false;

    if matches!(version, Some(1..=3)) {
        let converted = convert_legacy_binary_layout(record, path)?;
        *layout = converted;
        changes.push(format!(
            "upgraded legacy `paneLayout` version {} -> version {PANE_LAYOUT_VERSION_V4} n-ary split tree",
            version.unwrap_or(0)
        ));
        changed = true;
    }

    if let Some(root) = layout.get_mut("root")
        && rewrite_pane_tree_kinds(root, changes)
    {
        changed = true;
    }

    Ok(changed)
}

fn rewrite_pane_tree_kinds(node: &mut JsonValue, changes: &mut Vec<String>) -> bool {
    let Some(obj) = node.as_object_mut() else {
        return false;
    };
    match obj.get("type").and_then(JsonValue::as_str) {
        Some("pane") => {
            let kind = obj
                .get("kind")
                .and_then(JsonValue::as_str)
                .map(str::to_owned);
            let Some(kind) = kind else {
                return false;
            };
            if let Some(panel) = legacy_pane_kind_to_panel(&kind) {
                obj.insert("kind".to_owned(), JsonValue::String("extension".to_owned()));
                obj.insert("panel".to_owned(), JsonValue::String(panel.to_owned()));
                let id = obj
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("<pane>");
                changes.push(format!(
                    "paneLayout leaf `{id}`: converted legacy kind `{kind}` -> extension `{panel}`"
                ));
                return true;
            }
            false
        }
        Some("split") => {
            let mut changed = false;
            if let Some(JsonValue::Array(children)) = obj.get_mut("children") {
                for child in children {
                    if let Some(child_node) = child.get_mut("node")
                        && rewrite_pane_tree_kinds(child_node, changes)
                    {
                        changed = true;
                    }
                }
            }
            changed
        }
        _ => false,
    }
}

/// Port of `packages/hya-tui/src/state/legacyPaneLayout.ts` + `fromLegacy` +
/// `normalizePaneNode` so saved v1/v2/v3 binary trees migrate cleanly into v4.
fn convert_legacy_binary_layout(
    record: &JsonMap<String, JsonValue>,
    path: &Path,
) -> Result<JsonValue> {
    let version = record
        .get("version")
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let active = record
        .get("active")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| MigrationError::InvalidArtifact {
            path: path.to_path_buf(),
            detail: "legacy paneLayout is missing string `active`".to_owned(),
        })?
        .to_owned();
    let raw_root = record
        .get("root")
        .ok_or_else(|| MigrationError::InvalidArtifact {
            path: path.to_path_buf(),
            detail: "legacy paneLayout is missing `root`".to_owned(),
        })?
        .clone();

    let mut max_pane_num = 0u64;
    collect_max_pane_id(&raw_root, &mut max_pane_num);
    let mut next_pane = || {
        max_pane_num += 1;
        format!("pane-{max_pane_num}")
    };

    let upgraded_binary = match version {
        3 => migrate_conversation_dock(raw_root),
        1 | 2 => {
            let mut root = migrate_legacy_default(raw_root);
            if version == 1 {
                let sessions_leaf =
                    json!({ "type": "pane", "id": next_pane(), "kind": "sessions" });
                let todos_leaf = json!({ "type": "pane", "id": next_pane(), "kind": "todos" });
                let context_leaf = json!({ "type": "pane", "id": next_pane(), "kind": "context" });
                let projects_leaf =
                    json!({ "type": "pane", "id": next_pane(), "kind": "projects" });
                let right = json!({
                    "type": "split",
                    "axis": "horizontal",
                    "weight": 0.62,
                    "first": sessions_leaf,
                    "second": {
                        "type": "split",
                        "axis": "horizontal",
                        "weight": 0.36,
                        "first": todos_leaf,
                        "second": context_leaf,
                    }
                });
                root = json!({
                    "type": "split",
                    "axis": "vertical",
                    "weight": 0.1,
                    "first": projects_leaf,
                    "second": {
                        "type": "split",
                        "axis": "vertical",
                        "weight": 0.88,
                        "first": root,
                        "second": right,
                    }
                });
            }
            let conv_leaf = json!({ "type": "pane", "id": next_pane(), "kind": "conversation" });
            let act_leaf = json!({ "type": "pane", "id": next_pane(), "kind": "activity" });
            replace_conversation_with_dock(root, conv_leaf, act_leaf)
        }
        other => {
            return Err(MigrationError::InvalidArtifact {
                path: path.to_path_buf(),
                detail: format!("unsupported legacy paneLayout version `{other}`"),
            });
        }
    };

    let mut group_counter = 0u64;
    let nary = binary_to_nary(upgraded_binary, &mut group_counter)?;
    let normalized = normalize_nary_node(nary);
    Ok(json!({
        "version": PANE_LAYOUT_VERSION_V4,
        "root": normalized,
        "active": active,
    }))
}

fn collect_max_pane_id(node: &JsonValue, max_id: &mut u64) {
    let Some(obj) = node.as_object() else {
        return;
    };
    if let Some(id) = obj.get("id").and_then(JsonValue::as_str)
        && let Some(num) = id.strip_prefix("pane-").and_then(|s| s.parse::<u64>().ok())
    {
        *max_id = (*max_id).max(num);
    }
    if let Some(first) = obj.get("first") {
        collect_max_pane_id(first, max_id);
    }
    if let Some(second) = obj.get("second") {
        collect_max_pane_id(second, max_id);
    }
}

fn migrate_legacy_default(root: JsonValue) -> JsonValue {
    let is_legacy_default = root.as_object().is_some_and(|r| {
        r.get("type").and_then(JsonValue::as_str) == Some("split")
            && r.get("axis").and_then(JsonValue::as_str) == Some("vertical")
            && r.get("weight").and_then(JsonValue::as_f64) == Some(0.18)
            && r.get("first")
                .and_then(JsonValue::as_object)
                .is_some_and(|f| {
                    f.get("type").and_then(JsonValue::as_str) == Some("pane")
                        && f.get("kind").and_then(JsonValue::as_str) == Some("projects")
                })
            && r.get("second")
                .and_then(JsonValue::as_object)
                .is_some_and(|c| {
                    c.get("type").and_then(JsonValue::as_str) == Some("split")
                        && c.get("axis").and_then(JsonValue::as_str) == Some("vertical")
                        && c.get("weight").and_then(JsonValue::as_f64) == Some(0.74)
                        && c.get("first")
                            .and_then(JsonValue::as_object)
                            .is_some_and(|f| {
                                f.get("type").and_then(JsonValue::as_str) == Some("pane")
                                    && f.get("kind").and_then(JsonValue::as_str)
                                        == Some("conversation")
                            })
                })
    });
    if !is_legacy_default {
        return root;
    }
    let mut root = root;
    if let Some(r) = root.as_object_mut() {
        r.insert("weight".to_owned(), json!(0.1));
        if let Some(c) = r.get_mut("second").and_then(JsonValue::as_object_mut) {
            c.insert("weight".to_owned(), json!(0.88));
        }
    }
    root
}

fn migrate_conversation_dock(node: JsonValue) -> JsonValue {
    let Some(mut obj) = node.as_object().cloned() else {
        return node;
    };
    if obj.get("type").and_then(JsonValue::as_str) != Some("split") {
        return JsonValue::Object(obj);
    }
    let first = obj
        .remove("first")
        .map(migrate_conversation_dock)
        .unwrap_or(JsonValue::Null);
    let mut second = obj
        .remove("second")
        .map(migrate_conversation_dock)
        .unwrap_or(JsonValue::Null);

    let matches_dock = obj.get("sizing").is_none()
        && obj.get("axis").and_then(JsonValue::as_str) == Some("horizontal")
        && obj.get("weight").and_then(JsonValue::as_f64) == Some(0.8)
        && first.get("type").and_then(JsonValue::as_str) == Some("pane")
        && first.get("kind").and_then(JsonValue::as_str) == Some("conversation")
        && second.get("type").and_then(JsonValue::as_str) == Some("split")
        && second.get("sizing").is_none()
        && second.get("axis").and_then(JsonValue::as_str) == Some("horizontal")
        && second.get("weight").and_then(JsonValue::as_f64) == Some(0.2)
        && second
            .get("first")
            .and_then(|n| n.get("kind"))
            .and_then(JsonValue::as_str)
            == Some("activity")
        && second
            .get("second")
            .and_then(|n| n.get("kind"))
            .and_then(JsonValue::as_str)
            == Some("composer");

    if matches_dock {
        obj.insert("sizing".to_owned(), json!("content-second"));
        if let Some(sec_obj) = second.as_object_mut() {
            sec_obj.insert("sizing".to_owned(), json!("content-first"));
        }
    }
    obj.insert("first".to_owned(), first);
    obj.insert("second".to_owned(), second);
    JsonValue::Object(obj)
}

fn replace_conversation_with_dock(
    node: JsonValue,
    conv_leaf: JsonValue,
    act_leaf: JsonValue,
) -> JsonValue {
    let Some(mut obj) = node.as_object().cloned() else {
        return node;
    };
    if obj.get("type").and_then(JsonValue::as_str) == Some("pane") {
        if obj.get("kind").and_then(JsonValue::as_str) == Some("conversation") {
            obj.insert("kind".to_owned(), json!("composer"));
            return json!({
                "type": "split",
                "axis": "horizontal",
                "sizing": "content-second",
                "weight": 0.8,
                "first": conv_leaf,
                "second": {
                    "type": "split",
                    "axis": "horizontal",
                    "sizing": "content-first",
                    "weight": 0.2,
                    "first": act_leaf,
                    "second": JsonValue::Object(obj),
                }
            });
        }
        return JsonValue::Object(obj);
    }
    if let Some(first) = obj.remove("first") {
        let next = replace_conversation_with_dock(first, conv_leaf.clone(), act_leaf.clone());
        obj.insert("first".to_owned(), next);
    }
    if let Some(second) = obj.remove("second") {
        let next = replace_conversation_with_dock(second, conv_leaf, act_leaf);
        obj.insert("second".to_owned(), next);
    }
    JsonValue::Object(obj)
}

fn binary_to_nary(node: JsonValue, group_counter: &mut u64) -> Result<JsonValue> {
    let Some(obj) = node.as_object() else {
        return Ok(node);
    };
    if obj.get("type").and_then(JsonValue::as_str) == Some("pane") {
        return Ok(node);
    }
    let axis = obj
        .get("axis")
        .and_then(JsonValue::as_str)
        .unwrap_or("vertical");
    let direction = if axis == "vertical" { "row" } else { "column" };
    let sizing = obj.get("sizing").and_then(JsonValue::as_str);
    let weight = obj.get("weight").and_then(JsonValue::as_f64).unwrap_or(0.5);

    let first_raw = obj.get("first").cloned().unwrap_or(JsonValue::Null);
    let second_raw = obj.get("second").cloned().unwrap_or(JsonValue::Null);
    let first = binary_to_nary(first_raw, group_counter)?;
    let second = binary_to_nary(second_raw, group_counter)?;

    *group_counter += 1;
    let first_size = if sizing == Some("content-first") {
        json!({ "mode": "content" })
    } else {
        let w = if sizing == Some("content-second") {
            1.0
        } else {
            weight.max(f64::MIN_POSITIVE)
        };
        json!({ "mode": "weight", "value": w })
    };
    let second_size = if sizing == Some("content-second") {
        json!({ "mode": "content" })
    } else {
        let w = if sizing == Some("content-first") {
            1.0
        } else {
            (1.0 - weight).max(f64::MIN_POSITIVE)
        };
        json!({ "mode": "weight", "value": w })
    };

    Ok(json!({
        "type": "split",
        "id": format!("group-{group_counter}"),
        "direction": direction,
        "children": [
            { "node": first, "size": first_size },
            { "node": second, "size": second_size },
        ]
    }))
}

fn normalize_nary_node(node: JsonValue) -> JsonValue {
    let Some(mut obj) = node.as_object().cloned() else {
        return node;
    };
    if obj.get("type").and_then(JsonValue::as_str) != Some("split") {
        return JsonValue::Object(obj);
    }
    let direction = obj
        .get("direction")
        .and_then(JsonValue::as_str)
        .unwrap_or("row")
        .to_owned();
    let Some(JsonValue::Array(raw_children)) = obj.remove("children") else {
        return JsonValue::Object(obj);
    };

    let mut children: Vec<JsonValue> = Vec::with_capacity(raw_children.len());
    for child in raw_children {
        let Some(child_obj) = child.as_object() else {
            continue;
        };
        let Some(size) = child_obj.get("size") else {
            continue;
        };
        let nested = normalize_nary_node(child_obj.get("node").cloned().unwrap_or(JsonValue::Null));
        let same_dir_split = nested.get("type").and_then(JsonValue::as_str) == Some("split")
            && nested.get("direction").and_then(JsonValue::as_str) == Some(direction.as_str());
        if same_dir_split {
            let nested_children = nested
                .get("children")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default();
            if nested_children.is_empty() {
                continue;
            }
            let parent_mode = size.get("mode").and_then(JsonValue::as_str);
            if parent_mode == Some("weight")
                && nested_children.iter().all(|c| {
                    c.get("size")
                        .and_then(|s| s.get("mode"))
                        .and_then(JsonValue::as_str)
                        == Some("weight")
                })
            {
                let parent_w = size.get("value").and_then(JsonValue::as_f64).unwrap_or(1.0);
                let total: f64 = nested_children
                    .iter()
                    .filter_map(|c| c.get("size")?.get("value")?.as_f64())
                    .sum();
                for item in nested_children {
                    let item_w = item
                        .get("size")
                        .and_then(|s| s.get("value"))
                        .and_then(JsonValue::as_f64)
                        .unwrap_or(1.0);
                    let scaled = if total > 0.0 {
                        (parent_w * (item_w / total)).max(f64::MIN_POSITIVE)
                    } else {
                        1.0
                    };
                    children.push(json!({
                        "node": item.get("node").cloned().unwrap_or(JsonValue::Null),
                        "size": { "mode": "weight", "value": scaled }
                    }));
                }
                continue;
            }
            if parent_mode == Some("content") {
                for item in nested_children {
                    children.push(json!({
                        "node": item.get("node").cloned().unwrap_or(JsonValue::Null),
                        "size": { "mode": "content" }
                    }));
                }
                continue;
            }
        }
        children.push(json!({ "node": nested, "size": size }));
    }

    if children.len() == 1
        && let Some(sole) = children.pop()
        && let Some(sole_node) = sole.get("node").cloned()
    {
        return sole_node;
    }

    obj.insert("children".to_owned(), JsonValue::Array(children));
    JsonValue::Object(obj)
}
