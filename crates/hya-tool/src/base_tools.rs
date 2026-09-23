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
pub(crate) const TOOL_FAMILIES: [&str; 5] = [
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
    /// Identity of another tool family whose same-named tool this entry
    /// replaces whenever both families are loaded (for example channel-tools'
    /// mail-aware `wait` over extended-tools' `wait`). The only way two
    /// families may export one name: the winner is declared, never implied by
    /// load or lexicographic order.
    #[serde(default)]
    overrides: Option<String>,
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
    if let Some(tool) = policy.tools.iter().find(|tool| {
        tool.overrides
            .as_deref()
            .is_some_and(|target| target == identity || !TOOL_FAMILIES.contains(&target))
    }) {
        return Err(format!(
            "`{}` overrides `{}`, which is not another tool family",
            tool.name,
            tool.overrides.as_deref().unwrap_or_default()
        ));
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
    check_family_exports(
        &presets
            .iter()
            .map(|preset| (preset.identity.as_str(), preset.tools.as_slice()))
            .collect::<Vec<_>>(),
    )?;
    Ok(presets)
}

/// Every exported name (canonical or alias) belongs to exactly one family,
/// except a canonical name two families both export where exactly one entry
/// declares `overrides: <the other family>`.
fn check_family_exports(families: &[(&str, &[BaseToolExposure])]) -> Result<(), String> {
    let mut claims: std::collections::BTreeMap<&str, Vec<(&str, Option<&BaseToolExposure>)>> =
        std::collections::BTreeMap::new();
    for (identity, tools) in families {
        for tool in *tools {
            claims
                .entry(tool.name.as_str())
                .or_default()
                .push((identity, Some(tool)));
            for alias in &tool.aliases {
                claims
                    .entry(alias.name.as_str())
                    .or_default()
                    .push((identity, None));
            }
        }
    }
    for (name, owners) in &claims {
        match owners.as_slice() {
            [_] => {}
            [(left, Some(left_tool)), (right, Some(right_tool))]
                if (left_tool.overrides.as_deref() == Some(*right))
                    != (right_tool.overrides.as_deref() == Some(*left)) => {}
            _ => {
                return Err(format!(
                    "`{name}` is exported by more than one tool family without an `overrides` declaration"
                ));
            }
        }
    }
    Ok(())
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
    /// The tool family whose same-named tool this entry replaces when both
    /// are loaded.
    #[must_use]
    pub fn overrides(&self) -> Option<&str> {
        self.overrides.as_deref()
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn tools(yaml: &str) -> Vec<BaseToolExposure> {
        serde_norway::from_str(yaml).unwrap_or_else(|error| panic!("{yaml}: {error}"))
    }

    #[test]
    fn a_shared_name_needs_exactly_one_declared_override() {
        let extended = tools("- { name: wait, schema_version: 1, permission: read_only }");
        let plain = tools("- { name: wait, schema_version: 1, permission: read_only }");
        let overriding = tools(
            "- { name: wait, schema_version: 1, permission: read_only, overrides: hya/extended-tools }",
        );
        let wrong_target = tools(
            "- { name: wait, schema_version: 1, permission: read_only, overrides: hya/todo-tools }",
        );
        let accepted = check_family_exports(&[
            ("hya/extended-tools", &extended),
            ("hya/channel-tools", &overriding),
        ]);
        assert_eq!(accepted, Ok(()));
        for (label, other) in [("undeclared", &plain), ("wrong target", &wrong_target)] {
            let error = check_family_exports(&[
                ("hya/extended-tools", &extended),
                ("hya/channel-tools", other),
            ])
            .unwrap_err();
            assert!(error.contains("`wait`"), "{label}: {error}");
        }
        // Both sides claiming the override is as ambiguous as neither.
        let back = tools(
            "- { name: wait, schema_version: 1, permission: read_only, overrides: hya/channel-tools }",
        );
        assert!(
            check_family_exports(&[
                ("hya/extended-tools", &back),
                ("hya/channel-tools", &overriding),
            ])
            .is_err()
        );
        // Aliases can never be shared.
        let aliased = tools(
            "- { name: waiting, schema_version: 1, permission: read_only, aliases: [{ name: wait, visibility: hidden }] }",
        );
        assert!(
            check_family_exports(&[
                ("hya/extended-tools", &extended),
                ("hya/todo-tools", &aliased),
            ])
            .is_err()
        );
    }

    #[test]
    fn overrides_must_name_another_tool_family() {
        for target in ["hya/channel-tools", "acme/unknown"] {
            let policy: ExposurePolicy = serde_norway::from_str(&format!(
                "schema_version: 1\nidentity: hya/channel-tools\nprotected_names: []\ntools:\n  - {{ name: wait, schema_version: 1, permission: read_only, overrides: {target} }}\n"
            ))
            .unwrap_or_else(|error| panic!("{error}"));
            let error = validate_policy(&policy, "hya/channel-tools").unwrap_err();
            assert!(error.contains("overrides"), "{target}: {error}");
        }
    }
}
