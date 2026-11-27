//! In-memory Bundle source trees and directory loaders.
//!
//! Runtime code should embed/decode [`crate::PreparedCatalog`] bytes instead of
//! calling [`BundleSource::read_directory`] at process start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::BundleError;
use crate::model::{
    AgentRole, BundleIdentity, HandoffPolicy, ModelPolicy, PreparedChannelTemplate, ResourceView,
};

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

    /// Logical `/`-separated path relative to the bundle root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Raw file contents.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_parts(self) -> (String, Vec<u8>) {
        (self.path, self.bytes)
    }
}

/// Named collection of source files that prepare into one complete Bundle.
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

    /// Replace or add one logical file before preparing a generated package.
    ///
    /// Preparation still validates the normalized path, manifest closure, and
    /// exact resource bytes. This supports build tooling that injects a
    /// target-specific executable into a source bundle.
    #[must_use]
    pub fn with_file(mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let path = path.into();
        self.files.retain(|file| file.path != path);
        self.files.push(SourceFile::new(path, bytes));
        self
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

/// A bundle-owned argv check run against the source tree before publication.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceCheck {
    pub command: Vec<String>,
    #[serde(default = "default_check_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_check_timeout_secs() -> u64 {
    120
}

impl SourceCheck {
    pub(crate) fn validate(&self, source_name: &str) -> Result<(), crate::BundleError> {
        if self.command.is_empty() || self.command.iter().any(String::is_empty) {
            return Err(crate::BundleError::InvalidManifest {
                source_name: source_name.to_string(),
                detail: "check.command must be non-empty and contain no empty argv entries"
                    .to_string(),
            });
        }
        if self.timeout_secs == 0 || self.timeout_secs > 600 {
            return Err(crate::BundleError::InvalidManifest {
                source_name: source_name.to_string(),
                detail: "check.timeout_secs must be between 1 and 600".to_string(),
            });
        }
        Ok(())
    }
}

/// Strict source manifest shape for a closed Bundle.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceManifest {
    pub kind: String,
    /// Optional manifest schema version (`1` or `2`; defaults to `1`).
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Aggregate release version to resolve for this source manifest.
    #[serde(default)]
    pub version_ref: Option<String>,
    pub identity: BundleIdentity,
    /// Optional standardized runtime declaration (`static` | `js` | `native`).
    #[serde(default)]
    pub runtime: Option<SourceRuntime>,
    /// Optional source-root self-check.
    #[serde(default)]
    pub check: Option<SourceCheck>,
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
    /// Runtime data files materialized alongside the bundle (successor to `extensions.files`).
    #[serde(default)]
    pub assets: Vec<SourceResource>,
    /// HTTP endpoints served by the explicit process runtime.
    #[serde(default)]
    pub apis: Vec<SourceApi>,
    /// Session permission modes approved by the explicit process runtime.
    #[serde(default)]
    pub permission_modes: Vec<SourcePermissionMode>,
    /// Optional bundle-owned frontend extension.
    #[serde(default)]
    pub tui: Option<SourceTuiExtension>,
    /// The complete Agent set this bundle defines (inline or standalone `.md` file references).
    #[serde(default)]
    pub agents: Vec<SourceAgentEntry>,
    /// Channel policies/templates. Concrete channel ids are minted by the runtime.
    #[serde(default)]
    pub channels: Vec<PreparedChannelTemplate>,
    /// Optional packaged Workflow.
    #[serde(default)]
    pub workflow: Option<SourceWorkflow>,
}

/// Standardized runtime archetype (`runtime.kind`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceRuntimeKind {
    #[default]
    Static,
    Js,
    Native,
}

/// Execution mode for `runtime.kind: js`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceJsRuntimeMode {
    Managed,
    Process,
    Claude,
}

/// Native dynamic library capability declared in `runtime.capabilities`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceNativeCapability {
    Tools,
    Hooks,
    Rewriters,
}

/// Top-level `runtime:` block in `bundle.yaml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRuntime {
    #[serde(default)]
    pub kind: SourceRuntimeKind,
    #[serde(default)]
    pub mode: Option<SourceJsRuntimeMode>,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub abi_version: Option<u32>,
    #[serde(default)]
    pub crate_dir: Option<String>,
    #[serde(default)]
    pub library: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<SourceNativeCapability>,
}

/// One `apis:` entry: an HTTP endpoint the bundle's explicit
/// `extensions.process` answers through the plugin `api/request` request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceApi {
    /// Endpoint id (`[A-Za-z0-9._-]`, at most 64 bytes, starting alphanumeric).
    pub id: String,
    /// HTTP method (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`).
    pub method: crate::api::ApiMethod,
    /// Mount scope (`session` or `global`).
    pub scope: crate::api::ApiScope,
    /// Path template below the bundle mount (`/usage`, `/items/{id}`).
    pub path: String,
    /// Human-readable description shown in discovery listings.
    #[serde(default)]
    pub description: String,
    /// Declared extension file holding the request body's JSON Schema.
    #[serde(default)]
    pub request_schema: Option<String>,
    /// Declared extension file holding the response body's JSON Schema.
    #[serde(default)]
    pub response_schema: Option<String>,
}

/// One `permission_modes:` entry: a session permission mode whose asks the
/// bundle's explicit `extensions.process` answers through `permission.approve`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePermissionMode {
    /// Mode id (`[A-Za-z0-9._-]`, at most 64 bytes, starting alphanumeric).
    pub id: String,
    /// Short human-readable name shown in mode pickers.
    pub title: String,
    /// Optional longer description.
    #[serde(default)]
    pub description: String,
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

/// Workflow source declaration in a Bundle manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceWorkflow {
    /// Workflow-local identity that must equal the compiled name.
    pub id: String,
    /// Relative path to the Workflow Markdown document.
    pub path: String,
}

/// Either an inline agent declaration in `bundle.yaml` or a reference to a
/// standalone Markdown frontmatter file (`- path: agents/<id>.md`).
///
/// Deserialized natively through `serde_norway` so field paths and
/// `deny_unknown_fields` diagnostics stay exact.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAgentEntry {
    /// Relative path to a standalone `agents/<id>.md` frontmatter file.
    #[serde(default)]
    pub path: Option<String>,
    /// Stable agent id (required for inline definitions).
    #[serde(default)]
    pub id: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub role: Option<AgentRole>,
    pub color: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    /// Removed in 0.41.0: every subagent is a resident actor.
    #[serde(default)]
    pub spawn_lifecycle: Option<IgnoredAny>,
    pub resource_profile: Option<Value>,
    #[serde(default)]
    pub resource_view: ResourceView,
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default)]
    pub private: bool,
    // Validate strings during prepare: YAML otherwise coerces numeric scalars.
    #[serde(default)]
    pub advertise: Vec<Value>,
    #[serde(default)]
    pub handoff: HandoffPolicy,
    #[serde(default)]
    pub hook_refs: Vec<String>,
    /// Removed with the single-agent format; the tool plane is host-controlled.
    #[serde(default)]
    pub harness_access: Option<IgnoredAny>,
}

/// YAML frontmatter shape of a standalone `agents/<id>.md` file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAgentFile {
    #[serde(default)]
    pub id: Option<String>,
    /// Claude Code Plugin compatibility alias for `id`.
    #[serde(default)]
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub role: Option<AgentRole>,
    pub color: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    #[serde(default)]
    pub spawn_lifecycle: Option<IgnoredAny>,
    pub resource_profile: Option<Value>,
    #[serde(default)]
    pub resource_view: ResourceView,
    #[serde(default)]
    pub can_spawn: Vec<String>,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub advertise: Vec<Value>,
    #[serde(default)]
    pub handoff: HandoffPolicy,
    #[serde(default)]
    pub hook_refs: Vec<String>,
    #[serde(default)]
    pub harness_access: Option<IgnoredAny>,
}

#[derive(Debug)]
pub(crate) struct SourceAgent {
    /// Stable agent id.
    pub id: String,
    pub description: Option<String>,
    pub role: AgentRole,
    pub color: Option<String>,
    pub prompt: Option<String>,
    pub model_policy: ModelPolicy,
    pub workdir: Option<String>,
    /// Removed in 0.41.0: every subagent is a resident actor.
    pub spawn_lifecycle: Option<IgnoredAny>,
    pub resource_profile: Option<Value>,
    pub resource_view: ResourceView,
    pub can_spawn: Vec<String>,
    pub private: bool,
    pub advertise: Vec<Value>,
    pub handoff: HandoffPolicy,
    pub hook_refs: Vec<String>,
    /// Removed with the single-agent format; the tool plane is host-controlled.
    pub harness_access: Option<IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SourceResources {
    pub tools: Vec<SourceResource>,
    pub skills: Vec<SourceResource>,
    pub mcp: Vec<SourceResource>,
    pub hooks: Vec<SourceResource>,
    pub commands: Vec<SourceResource>,
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
    /// Raw native executables for a `kind: rust` process extension.
    pub rust: Vec<SourceResource>,
    /// Raw Rust dynamic libraries loaded in-process by the first-party tool loader.
    pub libraries: Vec<SourceResource>,
    /// Text assets materialized for process extensions or bundle MCP servers.
    pub files: Vec<SourceResource>,
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
    pub timeout_ms: Option<u64>,
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
/// as hya's `McpServerConfig` (stdio `command` argv or remote `url` with
/// optional `headers`).
///
/// The other fields pin the accepted shape under `deny_unknown_fields`.
/// Preparation preserves the raw resource text for the runtime to read.
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
    #[serde(default)]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    pub transport: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Permission tier declared in a structured tool descriptor (`tools/<id>.json`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceToolPermission {
    ReadOnly,
    Tool,
    Command,
    Interactive,
}

/// Alias entry in a structured tool descriptor: either a bare name or a
/// `{ name, visibility? }` object.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum SourceToolDescriptorAlias {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        visibility: Option<String>,
    },
}

/// Runtime entrypoint bindings declared in a structured tool descriptor.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceToolDescriptorEntrypoint {
    #[serde(default)]
    pub js: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub native: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub rpc: Option<String>,
}

/// Structured tool descriptor (`tools/<id>.json` or `tools/<id>.yaml`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceToolDescriptor {
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub name: String,
    pub description: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub permission: Option<SourceToolPermission>,
    #[serde(default)]
    pub aliases: Vec<SourceToolDescriptorAlias>,
    #[allow(dead_code)]
    #[serde(default)]
    pub prompt_hint: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub entrypoint: Option<SourceToolDescriptorEntrypoint>,
}

/// Multi-tool descriptor file (`tools/tools.yaml` or `tools/tools.json`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceToolDescriptorCollection {
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub tools: Vec<SourceToolDescriptor>,
}

/// Failure handling mode in a structured hook descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceHookFailureMode {
    FailClosed,
    FailOpen,
}

/// Shell command specification for a `command` hook handler.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum SourceHookCommand {
    Shell(String),
    Argv(Vec<String>),
}

/// Structured hook handler target (`js` | `rpc` | `command` | `native`).
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SourceHookHandler {
    Js { target: String },
    Rpc { method: String },
    Command { command: SourceHookCommand },
    Native { target: String },
}

/// One hook binding inside `hooks/hooks.json` or `hooks/<id>.json`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceHookEntry {
    pub event: String,
    #[serde(default)]
    pub matcher: Option<String>,
    pub handler: SourceHookHandler,
    #[serde(default)]
    pub failure_mode: Option<SourceHookFailureMode>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Structured hook descriptor (`hooks/hooks.json` or `hooks/<id>.yaml`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceHooksDescriptor {
    #[serde(default)]
    pub schema_version: Option<u32>,
    pub hooks: Vec<SourceHookEntry>,
}

pub(crate) struct ParsedSource {
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: SourceManifest,
    pub markdown_prompt: Option<String>,
}

/// Bundle-owned TUI entrypoint declaration, independent of frontend releases.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTuiExtension {
    /// Extension API generation (currently 1).
    pub api_version: u32,
    /// Relative TS/JS entrypoint declared in extension resources.
    pub entry: String,
    /// SDK contract version, not the frontend release version.
    pub sdk: String,
    /// Requested frontend capabilities.
    #[serde(default)]
    pub permissions: Vec<String>,
}
