//! Exposure policies for the trusted built-in native tool bundles.
//!
//! Each bundle's policy is read from its first-party bundle when the
//! process first needs it (structured `tools/*.{json,yaml}` descriptors for
//! ABI v2 bundles, or transitional `exposure.yaml` for legacy tool families);
//! nothing is compiled into the binary.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use hya_bundle::{Bundle, first_party_bundle};
use serde::Deserialize;

use hya_tool::{AliasVisibility, ToolPermission};

/// Core kernel identity that native bundles such as `hya/hashline` may override.
pub(crate) const BARE_CORE_IDENTITY: &str = "hya/bare";

/// Tool-family bundles in stable registry order.
pub(crate) const TOOL_FAMILIES: [&str; 5] = [
    "hya/hashline",
    "hya/coding-extras",
    "hya/extended-tools",
    "hya/network",
    "hya/agent-channels",
];

/// Metadata that owns one family's builtin visibility, aliases, and defaults.
pub struct BaseToolsPreset {
    schema_version: u32,
    identity: String,
    bundle_digest: String,
    prepared_catalog_bytes: &'static [u8],
    protected_names: Vec<String>,
    tools: Vec<BaseToolExposure>,
    file_operations_rule: Option<FileOperationsRule>,
}

/// A family's replacement for the kernel prompt's rule that sends file
/// operations to `bash`, used when the round offers one of `tools`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileOperationsRule {
    text: String,
    tools: Vec<String>,
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
    /// The tool's line in the system prompt's `<tools>` list (the text after
    /// `- <name>: `); a tool without one is not listed.
    #[serde(default)]
    prompt: Option<String>,
}

/// Alias metadata for one builtin tool.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseToolAlias {
    name: String,
    visibility: AliasVisibility,
}

/// The companion policy document of one tool-family bundle.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExposurePolicy {
    schema_version: u32,
    identity: String,
    protected_names: Vec<String>,
    tools: Vec<BaseToolExposure>,
    #[serde(default)]
    file_operations_rule: Option<FileOperationsRule>,
}

const fn default_exposed() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum DescriptorPermission {
    ReadOnly,
    Tool,
    Command,
    Task,
    Interactive,
}

impl From<DescriptorPermission> for ToolPermission {
    fn from(value: DescriptorPermission) -> Self {
        match value {
            DescriptorPermission::ReadOnly => Self::ReadOnly,
            DescriptorPermission::Tool | DescriptorPermission::Interactive => Self::Tool,
            DescriptorPermission::Command => Self::Command,
            DescriptorPermission::Task => Self::Task,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum V2ToolAlias {
    Name(String),
    Detailed {
        name: String,
        #[serde(default)]
        visibility: Option<AliasVisibility>,
    },
}

#[derive(Deserialize)]
struct V2ToolDescriptor {
    #[serde(default)]
    schema_version: Option<u32>,
    name: String,
    #[serde(default)]
    permission: Option<DescriptorPermission>,
    #[serde(default = "default_exposed")]
    exposed: bool,
    #[serde(default)]
    aliases: Vec<V2ToolAlias>,
    #[serde(default)]
    overrides: Option<String>,
    #[serde(default)]
    prompt_hint: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    protected: bool,
}

#[derive(Deserialize)]
struct V2ToolCollection {
    tools: Vec<V2ToolDescriptor>,
}

fn parse_v2_tool_descriptors(bundle: &Bundle) -> Result<ExposurePolicy, String> {
    let mut tools = Vec::with_capacity(bundle.tools().len());
    let mut protected_names = Vec::new();
    for resource in bundle.tools() {
        let value: serde_json::Value = if resource.source_path.ends_with(".json") {
            serde_json::from_str(&resource.content).map_err(|error| {
                format!("parse tool descriptor `{}`: {error}", resource.source_path)
            })?
        } else {
            serde_norway::from_str(&resource.content).map_err(|error| {
                format!("parse tool descriptor `{}`: {error}", resource.source_path)
            })?
        };
        let descriptor = if value.get("tools").is_some() {
            let collection: V2ToolCollection = serde_json::from_value(value).map_err(|error| {
                format!(
                    "decode tool descriptor collection `{}`: {error}",
                    resource.source_path
                )
            })?;
            collection
                .tools
                .into_iter()
                .find(|entry| entry.name == resource.local_id)
                .ok_or_else(|| {
                    format!(
                        "tool descriptor `{}` missing entry `{}`",
                        resource.source_path, resource.local_id
                    )
                })?
        } else {
            serde_json::from_value::<V2ToolDescriptor>(value).map_err(|error| {
                format!("decode tool descriptor `{}`: {error}", resource.source_path)
            })?
        };
        if descriptor.name != resource.local_id {
            return Err(format!(
                "tool descriptor `{}` declares `{}` but resource id is `{}`",
                resource.source_path, descriptor.name, resource.local_id
            ));
        }
        if descriptor.protected {
            protected_names.push(descriptor.name.clone());
        }
        let mut seen_aliases = BTreeSet::new();
        let mut aliases: Vec<BaseToolAlias> = descriptor
            .aliases
            .into_iter()
            .map(|alias| match alias {
                V2ToolAlias::Name(name) => BaseToolAlias {
                    name,
                    visibility: AliasVisibility::Hidden,
                },
                V2ToolAlias::Detailed { name, visibility } => BaseToolAlias {
                    name,
                    visibility: visibility.unwrap_or(AliasVisibility::Hidden),
                },
            })
            .filter(|alias| seen_aliases.insert(alias.name.clone()))
            .collect();
        for alias in &resource.aliases {
            if seen_aliases.insert(alias.clone()) {
                aliases.push(BaseToolAlias {
                    name: alias.clone(),
                    visibility: AliasVisibility::Hidden,
                });
            }
        }
        tools.push(BaseToolExposure {
            name: descriptor.name,
            schema_version: descriptor.schema_version.unwrap_or(1),
            permission: descriptor
                .permission
                .map(ToolPermission::from)
                .unwrap_or(ToolPermission::Tool),
            exposed: descriptor.exposed,
            aliases,
            overrides: descriptor
                .overrides
                .or_else(|| resource.overrides.clone())
                .or_else(|| bundle.overrides().map(str::to_owned)),
            prompt: descriptor.prompt_hint.or(descriptor.prompt),
        });
    }
    let file_operations_rule = (["ls", "glob", "find"]
        .iter()
        .all(|needed| tools.iter().any(|tool| tool.name == *needed)))
    .then(|| FileOperationsRule {
        text: "Use the file tools, not shell commands, to list, find, and search files".to_string(),
        tools: vec!["ls".to_string(), "glob".to_string(), "find".to_string()],
    });
    Ok(ExposurePolicy {
        schema_version: 2,
        identity: bundle.identity().id.clone(),
        protected_names,
        tools,
        file_operations_rule,
    })
}

fn load_preset(identity: &str) -> Result<BaseToolsPreset, String> {
    let catalog = first_party_bundle(identity).map_err(|error| error.to_string())?;
    let [bundle] = catalog.bundles() else {
        return Err("tool family must prepare one bundle".to_string());
    };
    if !bundle.agents().is_empty() || bundle.workflow().is_some() {
        return Err("tool family must be an agentless Bundle".to_string());
    }
    let policy = if let Some(asset) = bundle
        .extensions()
        .iter()
        .find(|asset| asset.local_id == "exposure")
    {
        serde_norway::from_str(&asset.content).map_err(|error| error.to_string())?
    } else if !bundle.tools().is_empty() {
        parse_v2_tool_descriptors(bundle)?
    } else {
        return Err("tool family has neither exposure policy nor resources.tools".to_string());
    };
    validate_policy(&policy, identity)?;
    Ok(BaseToolsPreset {
        schema_version: policy.schema_version,
        identity: policy.identity,
        bundle_digest: bundle.digest().to_string(),
        prepared_catalog_bytes: catalog.bytes(),
        protected_names: policy.protected_names,
        tools: policy.tools,
        file_operations_rule: policy.file_operations_rule,
    })
}

fn validate_policy(policy: &ExposurePolicy, identity: &str) -> Result<(), String> {
    if (policy.schema_version != 1 && policy.schema_version != 2) || policy.identity != identity {
        return Err(format!(
            "exposure policy must be schema 1 or 2 for `{identity}`, found `{}` v{}",
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
        if hya_tool::is_reserved_kernel_tool(&tool.name) {
            return Err(format!(
                "tool `{}` is permanently kernel-exclusive and cannot be declared by `{identity}`",
                tool.name
            ));
        }
        if tool.schema_version == 0 {
            return Err(format!("zero schema version for {}", tool.name));
        }
        if let Some(alias) = tool.aliases.iter().find(|alias| {
            hya_tool::is_reserved_kernel_tool(&alias.name) || !exports.insert(alias.name.as_str())
        }) {
            return Err(format!("duplicate or reserved alias {}", alias.name));
        }
    }
    if let Some(tool) = policy.tools.iter().find(|tool| {
        tool.overrides.as_deref().is_some_and(|target| {
            target == identity || (target != BARE_CORE_IDENTITY && !TOOL_FAMILIES.contains(&target))
        })
    }) {
        return Err(format!(
            "`{}` overrides `{}`, which is not `hya/bare` or another tool family",
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
    if let Some(tool) = policy.tools.iter().find(|tool| {
        tool.prompt
            .as_deref()
            .is_some_and(|line| line.trim().is_empty() || line.contains('\n'))
    }) {
        return Err(format!(
            "prompt of {} must be one non-empty line",
            tool.name
        ));
    }
    if let Some(rule) = &policy.file_operations_rule {
        if rule.text.trim().is_empty() || rule.text.contains('\n') {
            return Err("file_operations_rule text must be one non-empty line".to_string());
        }
        if let Some(tool) = rule
            .tools
            .iter()
            .find(|tool| !names.contains(tool.as_str()))
        {
            return Err(format!(
                "file_operations_rule tool is not canonical: {tool}"
            ));
        }
    }
    Ok(())
}

fn load_presets() -> Result<Vec<BaseToolsPreset>, String> {
    // These four independent packages include native binaries. Validate them
    // concurrently, then retain the declared family order for policy resolution.
    let presets = std::thread::scope(|scope| {
        let workers = TOOL_FAMILIES
            .iter()
            .map(|identity| {
                scope.spawn(move || {
                    load_preset(identity).map_err(|error| format!("{identity}: {error}"))
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| "tool-family validation worker panicked".to_string())?
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
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

static PRESETS: OnceLock<Result<Vec<BaseToolsPreset>, String>> = OnceLock::new();

fn cached_presets() -> Result<&'static [BaseToolsPreset], &'static str> {
    PRESETS
        .get_or_init(load_presets)
        .as_deref()
        .map_err(String::as_str)
}

/// Return every tool-family policy in stable family order.
///
/// # Panics
///
/// Panics when a trusted tool-family bundle is missing or its policy is
/// invalid; the builtin registry cannot be built without it.
#[must_use]
pub fn tool_bundle_presets() -> &'static [BaseToolsPreset] {
    cached_presets().unwrap_or_else(|error| panic!("load tool-family policy {error}"))
}

/// Whether `name` is a protected bare tool name that external aliases may not
/// mask. Always protects reserved kernel tools (`todo`, `rewrite`) and `read`
/// without touching `bundles/builtin/` on disk; also consults any already
/// loaded tool-family policies.
#[must_use]
pub fn is_protected_tool_name(name: &str) -> bool {
    if hya_tool::is_reserved_kernel_tool(name) || name == "read" {
        return true;
    }
    PRESETS
        .get()
        .and_then(|res| res.as_ref().ok())
        .is_some_and(|presets| presets.iter().any(|preset| preset.is_protected(name)))
}

/// The `<tools>` line the tool families or ABI v2 native tool bundles declare
/// for `name`; when two families export it, the line of the entry that
/// overrides the other. Returns `None` cleanly when tool bundles are absent.
#[must_use]
pub fn tool_prompt(name: &str) -> Option<&'static str> {
    cached_presets()
        .ok()
        .and_then(|presets| {
            presets
                .iter()
                .filter_map(|preset| preset.tool(name))
                .max_by_key(|tool| tool.overrides.is_some())
                .and_then(|tool| tool.prompt.as_deref())
        })
        .or_else(|| crate::v2_tools::v2_tool_prompt(name))
}

/// The rule a tool family declares in place of the kernel prompt's
/// file-operations rule, when `offered` accepts one of the tools it names.
/// Returns `None` cleanly when tool-family bundles are absent.
#[must_use]
pub fn file_operations_rule(offered: impl Fn(&str) -> bool) -> Option<&'static str> {
    cached_presets()
        .ok()?
        .iter()
        .filter_map(|preset| preset.file_operations_rule.as_ref())
        .find(|rule| rule.tools.iter().any(|tool| offered(tool)))
        .map(|rule| rule.text.as_str())
}

/// Return the foundational `hya/coding-extras` policy for compatibility.
#[must_use]
pub fn base_tools_preset() -> &'static BaseToolsPreset {
    tool_bundle_presets()
        .iter()
        .find(|preset| preset.identity() == "hya/coding-extras")
        .unwrap_or_else(|| &tool_bundle_presets()[0])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    #[allow(
        deprecated,
        reason = "Verify legacy public alias visibility still parses in transitional exposure.yaml"
    )]
    fn public_alias_parses_and_legacy_schemes_field_is_rejected() {
        let policy: ExposurePolicy = serde_norway::from_str(
            "schema_version: 1
identity: hya/base-tools
protected_names: []
tools:
  - name: read
    schema_version: 1
    permission: read_only
    aliases: [{ name: example_read, visibility: public }]
",
        )
        .unwrap();
        assert_eq!(validate_policy(&policy, "hya/base-tools"), Ok(()));
        assert_eq!(
            policy.tools[0].aliases[0].visibility(),
            AliasVisibility::Public
        );

        let with_schemes: Result<ExposurePolicy, _> = serde_norway::from_str(
            "schema_version: 1
identity: hya/base-tools
protected_names: []
schemes: [{ scheme: example, tool: read, writable: true }]
tools:
  - name: read
    schema_version: 1
    permission: read_only
",
        );
        assert!(with_schemes.is_err());
    }

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
            "- { name: wait, schema_version: 1, permission: read_only, overrides: hya/network }",
        );
        let accepted = check_family_exports(&[
            ("hya/extended-tools", &extended),
            ("hya/agent-channels", &overriding),
        ]);
        assert_eq!(accepted, Ok(()));
        for (label, other) in [("undeclared", &plain), ("wrong target", &wrong_target)] {
            let error = check_family_exports(&[
                ("hya/extended-tools", &extended),
                ("hya/agent-channels", other),
            ])
            .unwrap_err();
            assert!(error.contains("`wait`"), "{label}: {error}");
        }
        // Both sides claiming the override is as ambiguous as neither.
        let back = tools(
            "- { name: wait, schema_version: 1, permission: read_only, overrides: hya/agent-channels }",
        );
        assert!(
            check_family_exports(&[
                ("hya/extended-tools", &back),
                ("hya/agent-channels", &overriding),
            ])
            .is_err()
        );
        // Aliases can never be shared.
        let aliased = tools(
            "- { name: waiting, schema_version: 1, permission: read_only, aliases: [{ name: wait, visibility: hidden }] }",
        );
        assert!(
            check_family_exports(&[("hya/extended-tools", &extended), ("hya/network", &aliased),])
                .is_err()
        );
    }

    #[test]
    fn overrides_must_name_another_tool_family() {
        for target in ["hya/agent-channels", "acme/unknown"] {
            let policy: ExposurePolicy = serde_norway::from_str(&format!(
                "schema_version: 1\nidentity: hya/agent-channels\nprotected_names: []\ntools:\n  - {{ name: wait, schema_version: 1, permission: read_only, overrides: {target} }}\n"
            ))
            .unwrap_or_else(|error| panic!("{error}"));
            let error = validate_policy(&policy, "hya/agent-channels").unwrap_err();
            assert!(error.contains("overrides"), "{target}: {error}");
        }
    }

    #[test]
    fn embedded_tool_family_presets_match_runtime_bundles_and_registry_authority() {
        let presets = tool_bundle_presets();
        let expected_families: [(&str, u32, &[&str]); 5] = [
            ("hya/hashline", 2, &["read", "write", "edit", "grep"]),
            (
                "hya/coding-extras",
                2,
                &[
                    "ls",
                    "glob",
                    "find",
                    "apply_patch",
                    "ask_user",
                    "lsp",
                    "project_activity",
                ],
            ),
            (
                "hya/extended-tools",
                1,
                &["list_agents", "task", "search_agent", "archive", "wait"],
            ),
            ("hya/network", 2, &["webfetch", "websearch"]),
            (
                "hya/agent-channels",
                2,
                &["send", "list_channel", "report", "wait"],
            ),
        ];
        assert_eq!(presets.len(), expected_families.len());
        assert_eq!(base_tools_preset().identity(), "hya/coding-extras");

        for (preset, (identity, expected_schema_version, expected_names)) in
            presets.iter().zip(expected_families)
        {
            assert_eq!(preset.identity(), identity);
            assert_eq!(preset.schema_version(), expected_schema_version);
            assert_eq!(preset.bundle_digest().len(), 64);
            assert!(!preset.prepared_catalog_bytes().is_empty());

            let catalog = first_party_bundle(identity).unwrap_or_else(|e| panic!("{e}"));
            assert!(std::ptr::eq(
                preset.prepared_catalog_bytes(),
                catalog.bytes()
            ));
            assert_eq!(preset.bundle_digest(), catalog.bundles()[0].digest());

            let actual: BTreeSet<_> = preset.tools().iter().map(BaseToolExposure::name).collect();
            assert_eq!(
                actual,
                expected_names.iter().copied().collect(),
                "{identity}"
            );
        }

        let hashline = presets
            .iter()
            .find(|preset| preset.identity() == "hya/hashline")
            .unwrap();
        assert!(hashline.is_protected("read"));
        for tool in hashline.tools() {
            assert_eq!(tool.overrides(), Some("hya/bare"), "{}", tool.name());
        }

        let frozen: &[(&str, ToolPermission, &[&str])] = &[
            ("read", ToolPermission::ReadOnly, &[]),
            ("write", ToolPermission::Tool, &[]),
            ("edit", ToolPermission::Tool, &[]),
            ("ls", ToolPermission::ReadOnly, &[]),
            ("glob", ToolPermission::ReadOnly, &[]),
            ("find", ToolPermission::ReadOnly, &[]),
            ("grep", ToolPermission::ReadOnly, &[]),
            ("lsp", ToolPermission::ReadOnly, &[]),
            ("list_agents", ToolPermission::ReadOnly, &[]),
            ("task", ToolPermission::Task, &[]),
            ("project_activity", ToolPermission::ReadOnly, &[]),
            ("send", ToolPermission::Tool, &[]),
            ("list_channel", ToolPermission::ReadOnly, &[]),
            ("search_agent", ToolPermission::ReadOnly, &[]),
            ("report", ToolPermission::Tool, &[]),
            ("archive", ToolPermission::Task, &[]),
            ("wait", ToolPermission::ReadOnly, &[]),
            ("wait", ToolPermission::ReadOnly, &[]),
            ("ask_user", ToolPermission::Tool, &["question"]),
            ("apply_patch", ToolPermission::Tool, &["patch"]),
            ("webfetch", ToolPermission::Tool, &["fetch"]),
            ("websearch", ToolPermission::Tool, &["search"]),
        ];
        let mut actual_policy: Vec<_> = presets
            .iter()
            .flat_map(|p| p.tools())
            .map(|t| {
                (
                    t.name(),
                    t.permission(),
                    t.aliases()
                        .iter()
                        .map(BaseToolAlias::name)
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let mut expected_policy: Vec<_> = frozen
            .iter()
            .map(|(n, p, a)| (*n, *p, a.to_vec()))
            .collect();
        actual_policy.sort_by(|a, b| a.0.cmp(b.0));
        expected_policy.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(actual_policy, expected_policy);

        let registry = crate::builtins();
        let advertised: BTreeSet<_> = registry
            .schemas()
            .into_iter()
            .map(|s| s.name.as_str().to_string())
            .collect();
        let mut expected_exposed: BTreeSet<_> = presets
            .iter()
            .flat_map(|p| p.tools())
            .filter(|t| t.exposed())
            .map(|t| t.name().to_string())
            .collect();
        expected_exposed.insert("plan_exit".to_string());
        expected_exposed.insert("workflow".to_string());
        assert_eq!(advertised, expected_exposed);
        assert_eq!(
            registry.builtin_bundle_origin("plan_exit"),
            Some("hya/plan")
        );
        assert_eq!(
            registry.builtin_bundle_origin("workflow"),
            Some("hya/workflow")
        );
        assert_eq!(registry.resolve("plan").unwrap().tool.name(), "plan_exit");
        assert!(!advertised.contains("plan"));
        assert!(tool_prompt("plan_exit").is_some());
        assert!(tool_prompt("workflow").is_some());

        for tool in presets.iter().flat_map(|p| p.tools()) {
            assert!(
                tool.schema_version() == 1 || tool.schema_version() == 2,
                "{}",
                tool.name()
            );
            let resolved = registry.resolve(tool.name()).unwrap();
            assert_eq!(resolved.permission, tool.permission(), "{}", tool.name());
            for alias in tool.aliases() {
                let resolved_alias = registry.resolve(alias.name()).unwrap();
                assert_eq!(resolved_alias.tool.name(), tool.name());
                if alias.visibility() == AliasVisibility::Hidden {
                    assert!(!advertised.contains(alias.name()));
                }
            }
        }
    }
}
