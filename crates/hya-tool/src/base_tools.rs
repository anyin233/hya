//! Exposure policies for the five trusted tool-family bundles.
//!
//! Each family's `exposure.yaml` is read from its first-party bundle when the
//! process first needs it; nothing is compiled into the binary.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use hya_bundle::{PreparedBundleKind, first_party_bundle};
use serde::Deserialize;

use crate::tool::ToolPermission;

/// Tool-family bundles in stable registry order.
const TOOL_FAMILIES: [&str; 5] = [
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network-tools",
    "hya/channel-tools",
    "hya/todo-tools",
];

/// Metadata that owns one family's builtin visibility, aliases, and defaults.
pub struct BaseToolsPreset {
    schema_version: u32,
    identity: String,
    bundle_digest: String,
    prepared_catalog_bytes: &'static [u8],
    protected_names: Vec<String>,
    schemes: Vec<BaseToolScheme>,
    tools: Vec<BaseToolExposure>,
}

/// One builtin URI scheme declaration retained by the preset owner.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseToolScheme {
    scheme: String,
    tool: String,
    #[serde(default)]
    writable: bool,
}

/// Exposure policy for one Rust-implemented builtin.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseToolExposure {
    name: String,
    schema_version: u32,
    permission: ToolPermission,
    #[serde(default = "default_exposed")]
    exposed: bool,
    #[serde(default)]
    aliases: Vec<BaseToolAlias>,
}

/// An alternate exported spelling and whether models see its schema.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AliasVisibility {
    /// Dispatchable compatibility spelling omitted from advertised schemas.
    Hidden,
    /// Dispatchable spelling that is also advertised to models.
    Public,
}

/// Alias metadata for one builtin tool.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseToolAlias {
    name: String,
    visibility: AliasVisibility,
}

/// The companion `exposure.yaml` document of one tool-family bundle.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposurePolicy {
    schema_version: u32,
    identity: String,
    protected_names: Vec<String>,
    #[serde(default)]
    schemes: Vec<BaseToolScheme>,
    tools: Vec<BaseToolExposure>,
}

const fn default_exposed() -> bool {
    true
}

fn load_preset(identity: &str) -> Result<BaseToolsPreset, String> {
    let catalog = first_party_bundle(identity).map_err(|error| error.to_string())?;
    let [bundle] = catalog.bundles() else {
        return Err("tool family must prepare one bundle".to_string());
    };
    if bundle.kind() != PreparedBundleKind::Plugin {
        return Err("tool family must be a Plugin".to_string());
    }
    let asset = bundle
        .extensions()
        .iter()
        .find(|asset| asset.local_id == "exposure")
        .ok_or("tool family has no exposure policy")?;
    let policy: ExposurePolicy =
        serde_norway::from_str(&asset.content).map_err(|error| error.to_string())?;
    validate_policy(&policy, identity)?;
    Ok(BaseToolsPreset {
        schema_version: policy.schema_version,
        identity: policy.identity,
        bundle_digest: bundle.digest().to_string(),
        prepared_catalog_bytes: catalog.bytes(),
        protected_names: policy.protected_names,
        schemes: policy.schemes,
        tools: policy.tools,
    })
}

fn validate_policy(policy: &ExposurePolicy, identity: &str) -> Result<(), String> {
    if policy.schema_version != 1 || policy.identity != identity {
        return Err(format!(
            "exposure policy must be schema 1 for `{identity}`, found `{}` v{}",
            policy.identity, policy.schema_version
        ));
    }
    let names = policy
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    if names.len() != policy.tools.len() {
        return Err("duplicate canonical tool".to_string());
    }
    let mut exports = names.clone();
    for tool in &policy.tools {
        if tool.schema_version == 0 {
            return Err(format!("zero schema version for {}", tool.name));
        }
        if let Some(alias) = tool
            .aliases
            .iter()
            .find(|alias| !exports.insert(alias.name.as_str()))
        {
            return Err(format!("duplicate alias {}", alias.name));
        }
    }
    if let Some(protected) = policy
        .protected_names
        .iter()
        .find(|name| !names.contains(name.as_str()))
    {
        return Err(format!("protected name is not canonical: {protected}"));
    }
    if let Some(scheme) = policy
        .schemes
        .iter()
        .find(|scheme| !names.contains(scheme.tool.as_str()))
    {
        return Err(format!("scheme tool is not canonical: {}", scheme.tool));
    }
    Ok(())
}

fn load_presets() -> Result<Vec<BaseToolsPreset>, String> {
    let presets = TOOL_FAMILIES
        .iter()
        .map(|identity| load_preset(identity).map_err(|error| format!("{identity}: {error}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut exports = BTreeSet::new();
    for tool in presets.iter().flat_map(|preset| &preset.tools) {
        for name in std::iter::once(&tool.name).chain(tool.aliases.iter().map(|alias| &alias.name))
        {
            if !exports.insert(name.as_str()) {
                return Err(format!("`{name}` is exported by more than one tool family"));
            }
        }
    }
    Ok(presets)
}

impl BaseToolsPreset {
    /// Stable bundle identity.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
    /// Canonical prepared Plugin digest covering the manifest and policy asset.
    #[must_use]
    pub fn bundle_digest(&self) -> &str {
        &self.bundle_digest
    }
    /// Canonical prepared catalog bytes of the runtime-loaded bundle.
    #[must_use]
    pub fn prepared_catalog_bytes(&self) -> &[u8] {
        self.prepared_catalog_bytes
    }
    /// Version of the companion exposure-policy schema.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    /// Ordered builtin exposure declarations.
    #[must_use]
    pub fn tools(&self) -> &[BaseToolExposure] {
        &self.tools
    }
    /// URI schemes owned by builtin tools.
    #[must_use]
    pub fn schemes(&self) -> &[BaseToolScheme] {
        &self.schemes
    }
    /// Find a builtin declaration by canonical name.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&BaseToolExposure> {
        self.tools.iter().find(|tool| tool.name == name)
    }
    /// Whether runtime sources are forbidden from masking this name.
    #[must_use]
    pub fn is_protected(&self, name: &str) -> bool {
        self.protected_names
            .iter()
            .any(|protected| protected == name)
    }
}

impl BaseToolExposure {
    /// Canonical registry name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Version of the Rust implementation's advertised tool schema.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    /// Invocation permission posture.
    #[must_use]
    pub fn permission(&self) -> ToolPermission {
        self.permission
    }
    /// Whether this implementation is installed in the builtin registry.
    #[must_use]
    pub fn exposed(&self) -> bool {
        self.exposed
    }
    /// Alternate spellings for this tool.
    #[must_use]
    pub fn aliases(&self) -> &[BaseToolAlias] {
        &self.aliases
    }
}

impl BaseToolAlias {
    /// Alternate registry spelling.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Whether this spelling is advertised.
    #[must_use]
    pub fn visibility(&self) -> AliasVisibility {
        self.visibility
    }
}

impl BaseToolScheme {
    /// URI scheme text before `://`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }
    /// Canonical builtin tool that owns the scheme.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }
    /// Whether writes through this scheme are allowed.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.writable
    }
}

/// Return every tool-family policy in stable family order.
///
/// # Panics
///
/// Panics when a trusted tool-family bundle is missing or its policy is
/// invalid; the builtin registry cannot be built without it.
#[must_use]
pub fn tool_bundle_presets() -> &'static [BaseToolsPreset] {
    static PRESETS: OnceLock<Vec<BaseToolsPreset>> = OnceLock::new();
    PRESETS.get_or_init(|| {
        load_presets().unwrap_or_else(|error| panic!("load tool-family policy {error}"))
    })
}

/// Return the foundational `hya/base-tools` policy for compatibility.
#[must_use]
pub fn base_tools_preset() -> &'static BaseToolsPreset {
    &tool_bundle_presets()[0]
}
