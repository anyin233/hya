use std::path::Path;

use anyhow::Context as _;
use serde_norway::{Mapping, Value};

use super::file::{DEFAULT_CONFIG_YAML, config_path, parse_config};
use super::provider::{PROVIDER_KIND_LABELS, resolve_providers_filtered};

/// Upsert a non-secret OAuth provider route into `config.yaml`.
///
/// Creates the file (and parents) when missing. Preserves unrelated top-level
/// keys and the provider's other keys. Does **not** write secrets — tokens
/// live under `auth/<provider>.yaml`.
///
/// Existing user-authored model lists remain untouched. OAuth login creates an
/// empty model field for a new provider so the next startup can discover it;
/// fetched or guessed ids are never persisted here.
pub fn upsert_oauth_provider(
    config_path: &Path,
    provider_id: &str,
    kind: &str,
    base_url: &str,
) -> anyhow::Result<()> {
    upsert_provider_entry(config_path, provider_id, kind, base_url)
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

/// Failure of a `config.yaml` edit.
#[derive(Debug, thiserror::Error)]
pub enum ConfigEditError {
    /// The edit would produce an invalid config, or names something absent.
    #[error("{0}")]
    Invalid(String),
    /// The provider or model entry is not in `config.yaml`.
    #[error("{0}")]
    NotFound(String),
    /// Reading, parsing, or writing the file failed.
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}

/// Patch for one model's `models:` entry. Each field is optional: `None`
/// keeps the entry's current value (or keeps it absent). An empty (after
/// trimming) `display_name` removes `name`; a zero limit removes that
/// `limit.*` key (and an empty `limit:` map). `reasoning: Some(_)` writes a
/// boolean `reasoning` (`Some(true)` keeps a detailed `reasoning:` mapping);
/// there is no clear for `reasoning` here: remove the whole entry instead.
/// Keys this writer does not manage are always preserved.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelEntryOverride {
    /// Entry `name` (display name).
    pub display_name: Option<String>,
    /// Entry `limit.context`.
    pub context_limit: Option<u32>,
    /// Entry `limit.output`.
    pub output_limit: Option<u32>,
    /// Entry `reasoning: true|false`.
    pub reasoning: Option<bool>,
}

/// Non-secret facts about one provider declaration in `config.yaml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDeclaration {
    /// Provider id.
    pub id: String,
    /// `kind` label as written.
    pub kind: String,
    /// `base_url` as written.
    pub base_url: String,
    /// Whether the declaration has a non-empty inline `api_key`.
    pub inline_api_key: bool,
}

fn key(name: &str) -> Value {
    Value::String(name.to_string())
}

/// `config.yaml` text (the default document when the file is missing or
/// blank) and its parsed value.
struct ConfigSource {
    raw: String,
    root: Value,
}

fn read_config_source(config_path: &Path) -> anyhow::Result<ConfigSource> {
    let existing = if config_path.exists() {
        std::fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?
    } else {
        String::new()
    };
    let raw = if existing.trim().is_empty() {
        DEFAULT_CONFIG_YAML.to_string()
    } else {
        existing
    };
    let root: Value =
        serde_norway::from_str(&raw).with_context(|| format!("parse {}", config_path.display()))?;
    if root.as_mapping().is_none() {
        anyhow::bail!("config root must be a mapping");
    }
    Ok(ConfigSource { raw, root })
}

/// Read `config.yaml` as a YAML value (the default document when the file is
/// missing or blank).
fn read_config_value(config_path: &Path) -> anyhow::Result<Value> {
    read_config_source(config_path).map(|source| source.root)
}

/// Borrow `providers:` as a mapping, creating it when absent.
fn providers_mapping(root: &mut Value) -> anyhow::Result<&mut Mapping> {
    let map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("config root must be a mapping"))?;
    let slot = map
        .entry(key("providers"))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if slot.is_null() {
        *slot = Value::Mapping(Mapping::new());
    }
    slot.as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("providers must be a mapping"))
}

/// Validate and atomically write an edited config document.
///
/// `target` is the whole new document. The file text is edited minimally
/// ([`crate::config_edit`]): only the entries whose values change are
/// rewritten, so comments, blank lines, key order, quoting, and indentation
/// elsewhere are kept. The edited text must parse back to exactly `target`,
/// otherwise the edit is aborted and the file left untouched. When the file
/// uses YAML the minimal editor does not handle (anchors, aliases, tags,
/// several documents, multi-line flow or quoted values, …) the document is
/// re-rendered whole instead, dropping comments, and a warning is logged.
///
/// The result must parse and its providers must validate (secrets are not
/// expanded). The file is replaced through a temp file in the same directory
/// that inherits the old file's permissions.
fn write_config_value(
    config_path: &Path,
    source: &ConfigSource,
    target: &Value,
) -> Result<(), ConfigEditError> {
    let rendered = match crate::config_edit::minimal_edit(&source.raw, &source.root, target) {
        Ok(text) => {
            let reparsed: Value = serde_norway::from_str(&text).with_context(|| {
                format!(
                    "comment-preserving edit of {} produced invalid YAML; file left untouched",
                    config_path.display()
                )
            })?;
            if &reparsed != target {
                return Err(ConfigEditError::Io(anyhow::anyhow!(
                    "comment-preserving edit of {} did not match the intended change; \
                     file left untouched",
                    config_path.display()
                )));
            }
            text
        }
        Err(crate::config_edit::Unsupported(reason)) => {
            tracing::warn!(
                path = %config_path.display(),
                %reason,
                "config.yaml uses YAML the minimal editor does not handle; \
                 rewriting the whole file (comments and formatting are not kept)"
            );
            serde_norway::to_string(target).context("render updated hya config.yaml")?
        }
    };
    let file = parse_config(&rendered).map_err(|error| {
        ConfigEditError::Invalid(format!("edit would make config.yaml invalid: {error:#}"))
    })?;
    resolve_providers_filtered(&file, None, false).map_err(|error| {
        ConfigEditError::Invalid(format!("edit would make config.yaml invalid: {error:#}"))
    })?;
    let permissions = match std::fs::metadata(config_path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(anyhow::Error::from(error).into()),
    };
    crate::agent_model_config::atomic_replace(config_path, &rendered, permissions)?;
    Ok(())
}

/// Add or update `providers.<id>` with `kind` and `base_url` in
/// `config.yaml`, creating the file when missing. Every other key of the
/// provider (models, api_key, retry, …) and of the file is preserved; a new
/// provider gets `models: []` (discovery). Returns whether the provider was
/// created.
///
/// # Errors
/// [`ConfigEditError::Invalid`] for an unknown kind or an edit that fails
/// validation; [`ConfigEditError::Io`] for read/parse/write failures.
pub fn upsert_provider_entry(
    config_path: &Path,
    provider_id: &str,
    kind: &str,
    base_url: &str,
) -> Result<bool, ConfigEditError> {
    if !PROVIDER_KIND_LABELS.contains(&kind) {
        return Err(ConfigEditError::Invalid(format!(
            "unknown provider kind `{kind}` (expected one of {})",
            PROVIDER_KIND_LABELS.join(", ")
        )));
    }
    let _lock = crate::agent_model_config::lock_for(config_path)?;
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let providers = providers_mapping(&mut root)?;
    let created = !providers.contains_key(key(provider_id));
    let entry = providers
        .entry(key(provider_id))
        .or_insert_with(|| Value::Mapping(Mapping::new()));
    if !entry.is_mapping() {
        *entry = Value::Mapping(Mapping::new());
    }
    let provider = entry
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("provider entry must be a mapping"))?;
    provider.insert(key("kind"), Value::String(kind.to_string()));
    provider.insert(key("base_url"), Value::String(base_url.to_string()));
    if !provider.contains_key(key("models")) {
        provider.insert(key("models"), Value::Sequence(Vec::new()));
    }
    write_config_value(config_path, &source, &root)?;
    Ok(created)
}

fn model_entry_id(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(id) => Some(id.trim()),
        Value::Mapping(map) => map.get(key("id")).and_then(Value::as_str).map(str::trim),
        _ => None,
    }
}

/// Borrow `providers.<id>.models` as a sequence, creating it when absent.
fn provider_models<'a>(
    root: &'a mut Value,
    provider_id: &str,
) -> Result<&'a mut Vec<Value>, ConfigEditError> {
    let providers = providers_mapping(root)?;
    let provider = providers
        .get_mut(key(provider_id))
        .and_then(Value::as_mapping_mut)
        .ok_or_else(|| {
            ConfigEditError::NotFound(format!("provider not configured: {provider_id}"))
        })?;
    let models = provider
        .entry(key("models"))
        .or_insert_with(|| Value::Sequence(Vec::new()));
    if models.is_null() {
        *models = Value::Sequence(Vec::new());
    }
    models.as_sequence_mut().ok_or_else(|| {
        ConfigEditError::Invalid(format!("providers.{provider_id}.models must be a list"))
    })
}

/// Patch one model's entry in `providers.<provider_id>.models` (see
/// [`ModelEntryOverride`] for the field semantics), adding a bare `- <id>`
/// entry when the model has none. A string entry becomes a mapping when a
/// field is set; an entry left with only `id` is written back as a plain
/// string.
///
/// # Errors
/// [`ConfigEditError::NotFound`] when the provider is not declared;
/// [`ConfigEditError::Invalid`] when the patched file fails validation
/// (e.g. the merged `limit.output` exceeds the merged `limit.context`).
pub fn set_model_entry(
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
    metadata: &ModelEntryOverride,
) -> Result<(), ConfigEditError> {
    let _lock = crate::agent_model_config::lock_for(config_path)?;
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let models = provider_models(&mut root, provider_id)?;
    let position = models
        .iter()
        .position(|entry| model_entry_id(entry) == Some(model_id));
    let mut entry = match position.map(|index| models[index].clone()) {
        Some(Value::Mapping(map)) => map,
        _ => {
            let mut map = Mapping::new();
            map.insert(key("id"), Value::String(model_id.to_string()));
            map
        }
    };
    match metadata.display_name.as_deref().map(str::trim) {
        Some("") => {
            entry.remove(key("name"));
        }
        Some(name) => {
            entry.insert(key("name"), Value::String(name.to_string()));
        }
        None => {}
    }
    if metadata.context_limit.is_some() || metadata.output_limit.is_some() {
        let mut limit = match entry.remove(key("limit")) {
            Some(Value::Mapping(limit)) => limit,
            _ => Mapping::new(),
        };
        for (field, value) in [
            ("context", metadata.context_limit),
            ("output", metadata.output_limit),
        ] {
            match value {
                Some(0) => {
                    limit.remove(key(field));
                }
                Some(tokens) => {
                    limit.insert(key(field), Value::Number(u64::from(tokens).into()));
                }
                None => {}
            }
        }
        if !limit.is_empty() {
            entry.insert(key("limit"), Value::Mapping(limit));
        }
    }
    let detailed_reasoning = entry.get(key("reasoning")).is_some_and(Value::is_mapping);
    match metadata.reasoning {
        Some(false) => {
            entry.insert(key("reasoning"), Value::Bool(false));
        }
        Some(true) if !detailed_reasoning => {
            entry.insert(key("reasoning"), Value::Bool(true));
        }
        Some(true) | None => {}
    }
    let value = if entry.len() == 1 && entry.contains_key(key("id")) {
        Value::String(model_id.to_string())
    } else {
        Value::Mapping(entry)
    };
    match position {
        Some(index) => models[index] = value,
        None => models.push(value),
    }
    write_config_value(config_path, &source, &root)
}

/// Remove every `providers.<provider_id>.models` entry with `model_id`.
/// Returns whether an entry existed.
///
/// # Errors
/// [`ConfigEditError::NotFound`] when the provider is not declared.
pub fn remove_model_entry(
    config_path: &Path,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, ConfigEditError> {
    let _lock = crate::agent_model_config::lock_for(config_path)?;
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    let models = provider_models(&mut root, provider_id)?;
    let before = models.len();
    models.retain(|entry| model_entry_id(entry) != Some(model_id));
    if models.len() == before {
        return Ok(false);
    }
    write_config_value(config_path, &source, &root)?;
    Ok(true)
}

/// Remove `providers.<provider_id>` from `config.yaml`, keeping every other
/// key. Returns whether the provider was declared.
///
/// # Errors
/// [`ConfigEditError::Io`] for read/parse/write failures.
pub fn remove_provider_entry(
    config_path: &Path,
    provider_id: &str,
) -> Result<bool, ConfigEditError> {
    if !config_path.exists() {
        return Ok(false);
    }
    let _lock = crate::agent_model_config::lock_for(config_path)?;
    let source = read_config_source(config_path)?;
    let mut root = source.root.clone();
    if providers_mapping(&mut root)?
        .remove(key(provider_id))
        .is_none()
    {
        return Ok(false);
    }
    write_config_value(config_path, &source, &root)?;
    Ok(true)
}

/// Dotted paths of config values outside `providers` that name a model of
/// `provider_id` (`default_model: <id>/…`, `agents.<agent>.model`, …), so a
/// removal can warn about references it leaves dangling.
///
/// # Errors
/// Returns read or YAML parse failures.
pub fn provider_model_references(provider_id: &str) -> anyhow::Result<Vec<String>> {
    fn walk(value: &Value, path: &str, prefix: &str, out: &mut Vec<String>) {
        match value {
            Value::String(text) if text.trim().starts_with(prefix) => out.push(path.to_string()),
            Value::Mapping(map) => {
                for (name, child) in map {
                    let Some(name) = name.as_str() else { continue };
                    if path.is_empty() && name == "providers" {
                        continue;
                    }
                    let child_path = if path.is_empty() {
                        name.to_string()
                    } else {
                        format!("{path}.{name}")
                    };
                    walk(child, &child_path, prefix, out);
                }
            }
            Value::Sequence(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"), prefix, out);
                }
            }
            _ => {}
        }
    }
    let Some(path) = config_path() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    walk(
        &read_config_value(&path)?,
        "",
        &format!("{provider_id}/"),
        &mut out,
    );
    Ok(out)
}

/// Non-secret provider declarations from the active `config.yaml` (empty
/// when there is no config file).
///
/// # Errors
/// Returns read or YAML parse failures.
pub fn provider_declarations() -> anyhow::Result<Vec<ProviderDeclaration>> {
    let Some(path) = config_path() else {
        return Ok(Vec::new());
    };
    let root = read_config_value(&path)?;
    let Some(providers) = root.get("providers").and_then(Value::as_mapping) else {
        return Ok(Vec::new());
    };
    let text = |provider: &Mapping, field: &str| {
        provider
            .get(key(field))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    let mut out = providers
        .iter()
        .filter_map(|(id, provider)| {
            let id = id.as_str()?.to_string();
            let provider = provider.as_mapping()?;
            Some(ProviderDeclaration {
                kind: text(provider, "kind"),
                base_url: text(provider, "base_url"),
                inline_api_key: !text(provider, "api_key").is_empty(),
                id,
            })
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out)
}
