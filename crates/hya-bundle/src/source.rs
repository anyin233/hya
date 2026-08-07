//! In-memory AgentBundle source trees and directory loaders.
//!
//! Runtime code should embed/decode [`crate::PreparedCatalog`] bytes instead of
//! calling [`BundleSource::read_directory`] at process start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::BundleError;
use crate::model::{AgentRole, BundleIdentity, ModelPolicy, ResourceView, SpawnLifecycle};

/// One logical file in a bundle source: relative path plus raw bytes.
#[derive(Clone, Debug)]
pub struct SourceFile {
    path: String,
    bytes: Vec<u8>,
}

impl SourceFile {
    /// Build a source file from a logical path (`/`-separated) and its contents.
    #[must_use]
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }

    pub(crate) fn into_parts(self) -> (String, Vec<u8>) {
        (self.path, self.bytes)
    }
}

/// Named collection of source files that prepare into one or more bundles.
///
/// The `name` is diagnostic only (directory display path or package label).
#[derive(Clone, Debug)]
pub struct BundleSource {
    name: String,
    files: Vec<SourceFile>,
}

impl BundleSource {
    /// Wrap an in-memory file set under a diagnostic source name.
    #[must_use]
    pub fn new(name: impl Into<String>, files: Vec<SourceFile>) -> Self {
        Self {
            name: name.into(),
            files,
        }
    }

    /// Read one source directory for build-time preparation. Runtime code must
    /// embed and decode the resulting prepared bytes instead of calling this.
    pub fn read_directory(root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let root = root.as_ref();
        let mut paths = Vec::new();
        collect_directory(root, root, &mut paths)?;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let relative = path.strip_prefix(root).map_err(|error| BundleError::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            })?;
            let logical = relative
                .iter()
                .map(|component| component.to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| BundleError::InvalidSourcePath {
                    source_name: root.display().to_string(),
                    path: relative.display().to_string(),
                })?
                .join("/");
            let bytes = std::fs::read(&path).map_err(|error| BundleError::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            })?;
            files.push(SourceFile::new(logical, bytes));
        }
        Ok(Self::new(root.display().to_string(), files))
    }

    pub(crate) fn into_parts(self) -> (String, Vec<SourceFile>) {
        (self.name, self.files)
    }
}

fn collect_directory(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), BundleError> {
    let entries = std::fs::read_dir(dir).map_err(|error| BundleError::Io {
        path: dir.display().to_string(),
        detail: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| BundleError::Io {
            path: dir.display().to_string(),
            detail: error.to_string(),
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| BundleError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        if metadata.file_type().is_symlink() {
            return Err(BundleError::InvalidSourcePath {
                source_name: root.display().to_string(),
                path: path.display().to_string(),
            });
        }
        if metadata.is_dir() {
            collect_directory(root, &path, files)?;
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceManifest {
    pub kind: String,
    pub identity: BundleIdentity,
    #[serde(default)]
    pub resources: SourceResources,
    #[serde(default)]
    pub extensions: SourceExtensions,
    /// The one agent this bundle defines.
    pub agent: SourceAgent,
    /// Keys removed with the single-agent format. Captured only so prepare can
    /// name them; `deny_unknown_fields` alone gives an unhelpful serde message.
    #[serde(default)]
    pub api_version: Option<IgnoredAny>,
    #[serde(default)]
    pub agents: Option<IgnoredAny>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAgent {
    /// Stable agent id.
    pub id: String,
    pub description: Option<String>,
    pub role: AgentRole,
    pub color: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    #[serde(default)]
    pub spawn_lifecycle: SpawnLifecycle,
    pub resource_profile: Option<Value>,
    #[serde(default)]
    pub resource_view: ResourceView,
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default)]
    pub hook_refs: Vec<String>,
    /// Removed with the single-agent format; the tool plane is host-controlled.
    #[serde(default)]
    pub harness_access: Option<IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SourceResources {
    pub tools: Vec<SourceResource>,
    pub skills: Vec<SourceResource>,
    pub mcp: Vec<SourceResource>,
    pub hooks: Vec<SourceResource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceResource {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SourceExtensions {
    pub js: Vec<SourceResource>,
    pub rust: Vec<SourceResource>,
}

pub(crate) struct ParsedSource {
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: SourceManifest,
    pub markdown_prompt: Option<String>,
}
