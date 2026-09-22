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

/// Minimal source-manifest discriminator used before strict kind parsing.
#[derive(Debug, Deserialize)]
pub(crate) struct SourceKind {
    /// Manifest payload kind (`AgentBundle`, `AgentSetBundle`, or `WorkflowBundle`).
    pub kind: String,
}

/// Strict source manifest shape for the unchanged singular AgentBundle grammar.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAgentManifest {
    pub kind: String,
    pub identity: BundleIdentity,
    /// Provider-facing namespace for this bundle's tools and schemas; the
    /// identity name segment is the default.
    #[serde(default)]
    pub namespace: Option<String>,
    /// External URI-scheme extensions this bundle provides.
    #[serde(default)]
    pub schemas: Vec<SourceSchema>,
    #[serde(default)]
    pub resources: SourceResources,
    #[serde(default)]
    pub extensions: SourceExtensions,
    /// The one Agent this bundle defines.
    pub agent: SourceAgent,
    /// Keys removed with the single-agent format. Captured only so prepare can
    /// name them; `deny_unknown_fields` alone gives an unhelpful serde message.
    #[serde(default)]
    pub api_version: Option<IgnoredAny>,
    #[serde(default)]
    pub agents: Option<IgnoredAny>,
}

/// Strict source manifest shape for a closed AgentSetBundle.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAgentSetManifest {
    pub kind: String,
    pub identity: BundleIdentity,
    /// Provider-facing namespace for this bundle's tools and schemas.
    #[serde(default)]
    pub namespace: Option<String>,
    /// External URI-scheme extensions this bundle provides.
    #[serde(default)]
    pub schemas: Vec<SourceSchema>,
    #[serde(default)]
    pub resources: SourceResources,
    #[serde(default)]
    pub extensions: SourceExtensions,
    /// The complete Agent set this bundle defines.
    pub agents: Vec<SourceAgent>,
}

/// Strict source manifest shape for a WorkflowBundle.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceWorkflowManifest {
    pub kind: String,
    pub identity: BundleIdentity,
    /// Provider-facing namespace for this bundle's tools and schemas; the
    /// identity name segment is the default.
    #[serde(default)]
    pub namespace: Option<String>,
    /// The one Workflow source declaration owned by this bundle.
    pub workflow: SourceWorkflow,
    /// Candidate Agent set from which the exact compiled closure is selected.
    pub agents: Vec<SourceAgent>,
    /// External URI-scheme extensions this bundle provides.
    #[serde(default)]
    pub schemas: Vec<SourceSchema>,
    #[serde(default)]
    pub resources: SourceResources,
    #[serde(default)]
    pub extensions: SourceExtensions,
}

/// One `schemas:` entry: an external URI scheme served by a bundle-local tool.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceSchema {
    /// Scheme text as it appears before `://` (e.g. `db`).
    pub scheme: String,
    /// Bundle-local id of the tool that serves the scheme.
    pub tool: String,
    /// Whether the scheme accepts writes in addition to reads.
    #[serde(default)]
    pub writable: bool,
}

/// Workflow source declaration in a WorkflowBundle manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceWorkflow {
    /// Workflow-local identity that must equal the compiled name.
    pub id: String,
    /// Relative path to the Workflow Markdown document.
    pub path: String,
}

/// Strictly dispatched source manifest.
#[derive(Debug)]
pub(crate) enum SourceManifest {
    /// Singular AgentBundle manifest.
    Agent(Box<SourceAgentManifest>),
    /// Closed AgentSetBundle manifest.
    AgentSet(Box<SourceAgentSetManifest>),
    /// WorkflowBundle manifest.
    Workflow(Box<SourceWorkflowManifest>),
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
    /// The one optional out-of-process extension (`rust` | `bun` | `claude`).
    pub process: Option<SourceProcessExtension>,
}

/// The declared process extension: which runtime kind executes the bundle and
/// with what argv. Declared this phase; the unified spawn path lands later.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceProcessExtension {
    pub kind: SourceProcessKind,
    pub command: Vec<String>,
}

/// Supported process-extension runtime kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceProcessKind {
    Rust,
    Bun,
    Claude,
}

/// Connection shape of one `resources.mcp` declaration file: the same fields
/// as hya's `McpServerConfig` (stdio `command` argv or remote `url`).
///
/// The fields beyond `command`/`url` exist to pin the accepted shape under
/// `deny_unknown_fields`; the runtime spawn path consumes them in a later
/// phase, so they are unread here.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceMcpServer {
    #[serde(default)]
    pub command: Vec<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub env: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub url: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub transport: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub enabled: Option<bool>,
    #[allow(dead_code)]
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub(crate) struct ParsedSource {
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: SourceManifest,
    pub markdown_prompt: Option<String>,
}
