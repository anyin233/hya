//! Configuration-file model defaults for built-in and installed-bundle Agents.
//!
//! The owning file is selected from the immutable [`hya_core::AgentOrigin`].
//! Built-ins use the configured Hya file; bundle Agents use one file per bundle
//! under `agents/`, with the complete bundle identity encoded as one canonical
//! filesystem leaf. Updates lock a stable sidecar, reread the current document,
//! and atomically replace the target after changing only the selected model leaf.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, anyhow, bail};
use hya_core::{AgentModelConfiguration, AgentOrigin};
use hya_proto::ModelRef;
use serde_norway::{Mapping, Value};

static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// Files holding user-selected base models for built-in and bundle Agents.
///
/// Clones retain the same explicit global path and are safe to use from
/// independent controls. File updates coordinate through an on-disk lock, so
/// separate processes reread the latest YAML before each atomic replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentModelConfigFiles {
    global_file: PathBuf,
}

impl AgentModelConfigFiles {
    /// Construct model configuration storage rooted at `global_file`.
    #[must_use]
    pub fn new(global_file: PathBuf) -> Self {
        Self { global_file }
    }

    /// Return the explicit global Hya configuration path.
    #[must_use]
    pub fn global_path(&self) -> &Path {
        &self.global_file
    }

    /// Resolve the owning model configuration file for an Agent origin.
    ///
    /// Bundle identities are encoded as one canonical percent-encoded path
    /// segment. This keeps namespace separators such as `/` out of the
    /// filesystem hierarchy while retaining a reversible identity.
    ///
    /// # Errors
    ///
    /// Returns an error when a bundle identity is empty or cannot be represented
    /// as a non-empty UTF-8 filesystem leaf.
    pub fn path_for(&self, origin: AgentOrigin<'_>) -> anyhow::Result<PathBuf> {
        match origin {
            AgentOrigin::Builtin => Ok(self.global_file.clone()),
            AgentOrigin::Bundle { bundle_id } => {
                let leaf = encode_bundle_leaf(bundle_id)?;
                Ok(self
                    .global_file
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join("agents")
                    .join(leaf)
                    .join("config.yml"))
            }
        }
    }

    /// Load all model leaves from the global file and existing bundle files.
    ///
    /// Missing files and missing bundle directories are equivalent to an empty
    /// configuration. Only `agents.<id>.model` values affect the returned
    /// snapshot; all other YAML is retained by subsequent [`Self::set_model`]
    /// updates.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed YAML, malformed relevant `agents` fields,
    /// invalid bundle configuration directory leaves, or filesystem failures.
    pub fn load(&self) -> anyhow::Result<AgentModelConfiguration> {
        let builtin = self.read_models(&self.global_file)?.unwrap_or_default();
        let mut bundles = BTreeMap::new();

        let bundle_root = self
            .global_file
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("agents");
        let entries = match fs::read_dir(&bundle_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AgentModelConfiguration { builtin, bundles });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read bundle config directory {}", bundle_root.display())
                });
            }
        };

        for entry in entries {
            let entry = entry.with_context(|| {
                format!("read bundle config directory {}", bundle_root.display())
            })?;
            let file_type = entry.file_type().with_context(|| {
                format!("inspect bundle config entry {}", entry.path().display())
            })?;
            if !file_type.is_dir() {
                continue;
            }

            let config_path = entry.path().join("config.yml");
            let Some(models) = self.read_models(&config_path)? else {
                continue;
            };

            let leaf = entry.file_name();
            let leaf = leaf.to_str().ok_or_else(|| {
                anyhow!(
                    "bundle config directory {} has a non-UTF-8 identity leaf",
                    entry.path().display()
                )
            })?;
            let bundle_id = decode_bundle_leaf(leaf)
                .with_context(|| format!("validate bundle config directory leaf `{leaf}`"))?;
            if models.is_empty() {
                continue;
            }
            if bundles.insert(bundle_id.clone(), models).is_some() {
                bail!("duplicate bundle model configuration for `{bundle_id}`");
            }
        }

        Ok(AgentModelConfiguration { builtin, bundles })
    }

    /// Set or clear one Agent model in its owning configuration file.
    ///
    /// The current file is reread while a stable sidecar lock is held. The
    /// update changes only `agents.<agent_id>.model`; unrelated root keys and
    /// Agent settings remain untouched. A successful replacement uses a
    /// same-directory temporary file and rename, preserving existing file
    /// permissions and using private permissions for a newly created file.
    ///
    /// # Errors
    ///
    /// Returns an error when the owning YAML is malformed, a relevant field has
    /// the wrong type, the lock or filesystem operation fails, or serialization
    /// fails. Errors before the final rename leave the original file unchanged.
    pub fn set_model(
        &self,
        origin: AgentOrigin<'_>,
        agent_id: &str,
        model: Option<&ModelRef>,
    ) -> anyhow::Result<PathBuf> {
        let path = self.path_for(origin)?;
        let _parent = ensure_parent(&path)?;
        let _lock = lock_for(&path)?;

        let existing = match fs::read_to_string(&path) {
            Ok(raw) => Some(raw),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };

        let Some(existing_raw) = existing else {
            if model.is_none() {
                return Ok(path);
            }
            let mut root = Value::Mapping(Mapping::new());
            let _ = extract_models(&root, &path)?;
            if !update_model(&mut root, agent_id, model)? {
                return Ok(path);
            }
            let rendered = render_root(&root, &path)?;
            atomic_replace(&path, &rendered, None)?;
            return Ok(path);
        };

        let permissions = fs::metadata(&path)
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        let mut root = parse_document(&existing_raw, &path)?;
        let _ = extract_models(&root, &path)?;
        if !update_model(&mut root, agent_id, model)? {
            return Ok(path);
        }
        let rendered = render_root(&root, &path)?;
        atomic_replace(&path, &rendered, Some(permissions))?;
        Ok(path)
    }

    fn read_models(&self, path: &Path) -> anyhow::Result<Option<BTreeMap<String, ModelRef>>> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let root = parse_document(&raw, path)?;
        extract_models(&root, path).map(Some)
    }
}

fn key(name: &str) -> Value {
    Value::String(name.to_string())
}

fn parse_document(raw: &str, path: &Path) -> anyhow::Result<Value> {
    let root = if raw.trim().is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        serde_norway::from_str(raw).with_context(|| format!("parse {}", path.display()))?
    };
    if !matches!(root, Value::Mapping(_)) {
        bail!("configuration root {} must be a mapping", path.display());
    }
    Ok(root)
}

fn extract_models(root: &Value, path: &Path) -> anyhow::Result<BTreeMap<String, ModelRef>> {
    let Some(agents_value) = root.as_mapping().and_then(|map| map.get(key("agents"))) else {
        return Ok(BTreeMap::new());
    };
    let agents = agents_value.as_mapping().ok_or_else(|| {
        anyhow!(
            "configuration agents field {} must be a mapping",
            path.display()
        )
    })?;

    let mut models = BTreeMap::new();
    for (agent_key, agent_value) in agents {
        let agent_id = agent_key.as_str().ok_or_else(|| {
            anyhow!(
                "configuration agents key in {} must be a string",
                path.display()
            )
        })?;
        let agent = agent_value.as_mapping().ok_or_else(|| {
            anyhow!(
                "configuration agents.{agent_id} in {} must be a mapping",
                path.display()
            )
        })?;
        let Some(model_value) = agent.get(key("model")) else {
            continue;
        };
        match model_value {
            Value::Null => {}
            Value::String(model) => {
                models.insert(agent_id.to_string(), ModelRef::new(model.clone()));
            }
            _ => {
                bail!(
                    "configuration agents.{agent_id}.model in {} must be a string or null",
                    path.display()
                );
            }
        }
    }
    Ok(models)
}

fn update_model(
    root: &mut Value,
    agent_id: &str,
    model: Option<&ModelRef>,
) -> anyhow::Result<bool> {
    let root_map = root
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("configuration root must be a mapping"))?;
    let agents_key = key("agents");
    let Some(agents_value) = root_map.get_mut(&agents_key) else {
        if model.is_none() {
            return Ok(false);
        }
        root_map.insert(agents_key.clone(), Value::Mapping(Mapping::new()));
        let Some(agents_value) = root_map.get_mut(&agents_key) else {
            bail!("failed to create configuration agents mapping");
        };
        return update_model_in_agents(agents_value, agent_id, model);
    };
    update_model_in_agents(agents_value, agent_id, model)
}

fn update_model_in_agents(
    agents_value: &mut Value,
    agent_id: &str,
    model: Option<&ModelRef>,
) -> anyhow::Result<bool> {
    let agents = agents_value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("configuration agents field must be a mapping"))?;
    let agent_key = key(agent_id);
    let model_key = key("model");
    let Some(agent_value) = agents.get_mut(&agent_key) else {
        let Some(model) = model else {
            return Ok(false);
        };
        let mut agent = Mapping::new();
        agent.insert(model_key, Value::String(model.as_str().to_string()));
        agents.insert(agent_key, Value::Mapping(agent));
        return Ok(true);
    };

    let agent = agent_value
        .as_mapping_mut()
        .ok_or_else(|| anyhow!("configuration agents.{agent_id} must be a mapping"))?;
    match model {
        Some(model) => {
            let value = Value::String(model.as_str().to_string());
            if agent.get(&model_key) == Some(&value) {
                Ok(false)
            } else {
                agent.insert(model_key, value);
                Ok(true)
            }
        }
        None => Ok(agent.remove(&model_key).is_some()),
    }
}

fn render_root(root: &Value, path: &Path) -> anyhow::Result<String> {
    serde_norway::to_string(root).with_context(|| format!("render {}", path.display()))
}

fn ensure_parent(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(&parent)
            .with_context(|| format!("create configuration directory {}", parent.display()))?;
    }
    Ok(parent)
}

fn lock_for(path: &Path) -> anyhow::Result<File> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("configuration path {} has no file name", path.display()))?
        .to_string_lossy();
    let lock_path = parent.join(format!(".{file_name}.lock"));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(&lock_path)
        .with_context(|| format!("open configuration lock {}", lock_path.display()))?;
    set_private_permissions(&lock_path)
        .with_context(|| format!("protect configuration lock {}", lock_path.display()))?;
    file.lock()
        .with_context(|| format!("lock configuration {}", path.display()))?;
    Ok(file)
}

fn atomic_replace(
    path: &Path,
    contents: &str,
    permissions: Option<std::fs::Permissions>,
) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("configuration path {} has no file name", path.display()))?
        .to_string_lossy();

    let (temporary_path, mut temporary) = create_temporary(parent, &file_name)?;
    let result = (|| -> anyhow::Result<()> {
        temporary.write_all(contents.as_bytes()).with_context(|| {
            format!("write temporary configuration {}", temporary_path.display())
        })?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temporary_path, permissions)
                .with_context(|| format!("preserve permissions on {}", temporary_path.display()))?;
        } else {
            set_private_permissions(&temporary_path).with_context(|| {
                format!("protect new configuration {}", temporary_path.display())
            })?;
        }
        temporary.sync_all().with_context(|| {
            format!("sync temporary configuration {}", temporary_path.display())
        })?;
        drop(temporary);
        fs::rename(&temporary_path, path)
            .with_context(|| format!("replace configuration {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_temporary(parent: &Path, file_name: &str) -> anyhow::Result<(PathBuf, File)> {
    for _ in 0..1024 {
        let serial = NEXT_TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{file_name}.tmp-{}-{serial}", std::process::id()));
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create temporary configuration {}", candidate.display())
                });
            }
        }
    }
    bail!(
        "could not allocate a temporary configuration file beside {}",
        parent.display()
    );
}

fn set_private_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)
    }
}

fn encode_bundle_leaf(bundle_id: &str) -> anyhow::Result<String> {
    if bundle_id.is_empty() {
        bail!("bundle identity must not be empty");
    }
    let special_dot = matches!(bundle_id, "." | "..");
    let mut encoded = String::with_capacity(bundle_id.len());
    for byte in bundle_id.as_bytes() {
        if !special_dot && is_unreserved(*byte) {
            encoded.push(char::from(*byte));
        } else {
            push_percent_encoded(&mut encoded, *byte);
        }
    }
    if encoded.is_empty() {
        bail!("bundle identity must produce a non-empty path leaf");
    }
    Ok(encoded)
}

fn decode_bundle_leaf(leaf: &str) -> anyhow::Result<String> {
    if leaf.is_empty() {
        bail!("bundle configuration directory leaf must not be empty");
    }
    let bytes = leaf.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    bail!("bundle configuration leaf `{leaf}` has an incomplete escape");
                }
                let high = decode_hex(bytes[index + 1])?;
                let low = decode_hex(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte if is_unreserved(byte) => {
                decoded.push(byte);
                index += 1;
            }
            _ => {
                bail!("bundle configuration leaf `{leaf}` contains an unescaped byte");
            }
        }
    }
    let bundle_id = String::from_utf8(decoded)
        .with_context(|| format!("bundle configuration leaf `{leaf}` is not UTF-8"))?;
    let canonical = encode_bundle_leaf(&bundle_id)?;
    if canonical != leaf {
        bail!("bundle configuration leaf `{leaf}` is not canonical (use `{canonical}`)");
    }
    Ok(bundle_id)
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn push_percent_encoded(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push('%');
    output.push(char::from(HEX[(byte >> 4) as usize]));
    output.push(char::from(HEX[(byte & 0x0F) as usize]));
}

fn decode_hex(byte: u8) -> anyhow::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid percent escape byte 0x{byte:02X}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hya-agent-model-config-{now}-{serial}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn files() -> (PathBuf, AgentModelConfigFiles) {
        let dir = tempdir();
        let global = dir.join("hya/config.yaml");
        (dir, AgentModelConfigFiles::new(global))
    }

    #[test]
    fn absent_files_load_as_empty_configuration() {
        let (_dir, files) = files();
        let loaded = files.load().unwrap();
        assert!(loaded.builtin.is_empty());
        assert!(loaded.bundles.is_empty());
    }

    #[test]
    fn bundle_leaf_encoding_is_reversible_and_canonical() {
        let (_dir, files) = files();
        let cases = [
            ("simple-bundle", "simple-bundle"),
            ("org/foo", "org%2Ffoo"),
            ("../escape", "..%2Fescape"),
            (".", "%2E"),
            ("..", "%2E%2E"),
            ("literal%2F", "literal%252F"),
        ];
        for (bundle_id, leaf) in cases {
            let path = files.path_for(AgentOrigin::Bundle { bundle_id }).unwrap();
            assert_eq!(path.file_name().unwrap().to_str().unwrap(), "config.yml");
            assert!(path.parent().unwrap().ends_with(leaf));
            assert_eq!(decode_bundle_leaf(leaf).unwrap(), bundle_id);
        }
        assert!(decode_bundle_leaf("org%2ffoo").is_err());
        assert!(decode_bundle_leaf("org/foo").is_err());
        assert_ne!(
            files
                .path_for(AgentOrigin::Bundle {
                    bundle_id: "org/foo"
                })
                .unwrap(),
            files
                .path_for(AgentOrigin::Bundle {
                    bundle_id: "org%2Ffoo",
                })
                .unwrap()
        );
    }

    #[test]
    fn load_rejects_noncanonical_bundle_directory_alias() {
        let (_dir, files) = files();
        let alias = files
            .global_path()
            .parent()
            .unwrap()
            .join("agents/org%2ffoo");
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::write(
            alias.join("config.yml"),
            "agents:\n  worker:\n    model: provider/model\n",
        )
        .unwrap();
        assert!(files.load().is_err());
    }

    #[test]
    fn set_reload_preserves_unknown_yaml_and_owns_bundle_files() {
        let (_dir, files) = files();
        let global = files.global_path().to_path_buf();
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(
            &global,
            "name: preserve-me\nagents:\n  shared:\n    model: old/global\n    category: deep\n  untouched:\n    note: yes\nproviders:\n  p:\n    base_url: https://example.invalid\n",
        )
        .unwrap();

        let bundle_origin = AgentOrigin::Bundle {
            bundle_id: "org/foo",
        };
        let bundle_path = files.path_for(bundle_origin).unwrap();
        files
            .set_model(
                AgentOrigin::Builtin,
                "shared",
                Some(&ModelRef::new("new/global")),
            )
            .unwrap();
        files
            .set_model(
                AgentOrigin::Bundle {
                    bundle_id: "org/foo",
                },
                "shared",
                Some(&ModelRef::new("new/bundle")),
            )
            .unwrap();

        let global_raw = std::fs::read_to_string(&global).unwrap();
        assert!(global_raw.contains("name: preserve-me"));
        assert!(global_raw.contains("category: deep"));
        assert!(global_raw.contains("new/global"));
        assert!(!global_raw.contains("new/bundle"));
        assert!(bundle_path.exists());
        let loaded = files.load().unwrap();
        assert_eq!(
            loaded.builtin.get("shared"),
            Some(&ModelRef::new("new/global"))
        );
        assert_eq!(
            loaded
                .bundles
                .get("org/foo")
                .and_then(|models| models.get("shared")),
            Some(&ModelRef::new("new/bundle"))
        );
    }

    #[test]
    fn clearing_removes_only_model_leaf_and_malformed_input_stays_unchanged() {
        let (_dir, files) = files();
        let global = files.global_path().to_path_buf();
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        let original = "agents:\n  target:\n    model: provider/model\n    category: deep\n  other:\n    model: keep/me\nroot_unknown: value\n";
        std::fs::write(&global, original).unwrap();
        files
            .set_model(AgentOrigin::Builtin, "target", None)
            .unwrap();
        let cleared = std::fs::read_to_string(&global).unwrap();
        assert!(!cleared.contains("provider/model"));
        assert!(cleared.contains("category: deep"));
        assert!(cleared.contains("keep/me"));
        assert!(cleared.contains("root_unknown: value"));

        let malformed = "agents:\n  target:\n    model: [not, a, string]\nkeep: exact\n";
        std::fs::write(&global, malformed).unwrap();
        let error = files.set_model(
            AgentOrigin::Builtin,
            "target",
            Some(&ModelRef::new("provider/new")),
        );
        assert!(error.is_err());
        assert_eq!(std::fs::read_to_string(&global).unwrap(), malformed);
    }

    #[test]
    fn concurrent_updates_reread_latest_document() {
        let (_dir, files) = files();
        let left = files.clone();
        let right = files.clone();
        let first = thread::spawn(move || {
            left.set_model(
                AgentOrigin::Builtin,
                "left",
                Some(&ModelRef::new("provider/left")),
            )
            .unwrap();
        });
        let second = thread::spawn(move || {
            right
                .set_model(
                    AgentOrigin::Builtin,
                    "right",
                    Some(&ModelRef::new("provider/right")),
                )
                .unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();

        let loaded = files.load().unwrap();
        assert_eq!(
            loaded.builtin.get("left"),
            Some(&ModelRef::new("provider/left"))
        );
        assert_eq!(
            loaded.builtin.get("right"),
            Some(&ModelRef::new("provider/right"))
        );
    }

    #[test]
    fn existing_permissions_are_preserved_and_new_files_are_private() {
        let (_dir, files) = files();
        let path = files.global_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "root: value\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640)).unwrap();
            files
                .set_model(
                    AgentOrigin::Builtin,
                    "target",
                    Some(&ModelRef::new("provider/model")),
                )
                .unwrap();
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }

        let (_other_dir, new_files) = self::files();
        new_files
            .set_model(
                AgentOrigin::Builtin,
                "target",
                Some(&ModelRef::new("provider/model")),
            )
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(new_files.global_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
