//! Embedded exposure policies for the five trusted tool-family presets.

use crate::tool::ToolPermission;

/// Metadata that owns one family's builtin visibility, aliases, and defaults.
pub struct BaseToolsPreset {
    schema_version: u32,
    identity: &'static str,
    bundle_digest: &'static str,
    prepared_catalog_bytes: &'static [u8],
    protected_names: &'static [&'static str],
    schemes: &'static [BaseToolScheme],
    tools: &'static [BaseToolExposure],
}

/// One builtin URI scheme declaration retained by the preset owner.
pub struct BaseToolScheme {
    scheme: &'static str,
    tool: &'static str,
    writable: bool,
}

/// Exposure policy for one Rust-implemented builtin.
pub struct BaseToolExposure {
    name: &'static str,
    schema_version: u32,
    permission: ToolPermission,
    exposed: bool,
    aliases: &'static [BaseToolAlias],
}

/// An alternate exported spelling and whether models see its schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AliasVisibility {
    /// Dispatchable compatibility spelling omitted from advertised schemas.
    Hidden,
    /// Dispatchable spelling that is also advertised to models.
    Public,
}

/// Alias metadata for one builtin tool.
pub struct BaseToolAlias {
    name: &'static str,
    visibility: AliasVisibility,
}

include!(concat!(env!("OUT_DIR"), "/base_tools_preset.rs"));

impl BaseToolsPreset {
    /// Stable bundle identity.
    #[must_use]
    pub const fn identity(&self) -> &str {
        self.identity
    }
    /// Canonical prepared Plugin digest covering the manifest and policy asset.
    #[must_use]
    pub const fn bundle_digest(&self) -> &str {
        self.bundle_digest
    }
    /// Canonical prepared catalog bytes embedded at build time.
    #[must_use]
    pub const fn prepared_catalog_bytes(&self) -> &[u8] {
        self.prepared_catalog_bytes
    }
    /// Version of the companion exposure-policy schema.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }
    /// Ordered builtin exposure declarations.
    #[must_use]
    pub const fn tools(&self) -> &[BaseToolExposure] {
        self.tools
    }
    /// URI schemes owned by builtin tools.
    #[must_use]
    pub const fn schemes(&self) -> &[BaseToolScheme] {
        self.schemes
    }
    /// Find a builtin declaration by canonical name.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&BaseToolExposure> {
        self.tools.iter().find(|tool| tool.name == name)
    }
    /// Whether runtime sources are forbidden from masking this name.
    #[must_use]
    pub fn is_protected(&self, name: &str) -> bool {
        self.protected_names.contains(&name)
    }
}

impl BaseToolExposure {
    /// Canonical registry name.
    #[must_use]
    pub const fn name(&self) -> &str {
        self.name
    }
    /// Version of the Rust implementation's advertised tool schema.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }
    /// Invocation permission posture.
    #[must_use]
    pub const fn permission(&self) -> ToolPermission {
        self.permission
    }
    /// Whether this implementation is installed in the builtin registry.
    #[must_use]
    pub const fn exposed(&self) -> bool {
        self.exposed
    }
    /// Alternate spellings for this tool.
    #[must_use]
    pub const fn aliases(&self) -> &[BaseToolAlias] {
        self.aliases
    }
}

impl BaseToolAlias {
    /// Alternate registry spelling.
    #[must_use]
    pub const fn name(&self) -> &str {
        self.name
    }
    /// Whether this spelling is advertised.
    #[must_use]
    pub const fn visibility(&self) -> AliasVisibility {
        self.visibility
    }
}

impl BaseToolScheme {
    /// URI scheme text before `://`.
    #[must_use]
    pub const fn scheme(&self) -> &str {
        self.scheme
    }
    /// Canonical builtin tool that owns the scheme.
    #[must_use]
    pub const fn tool(&self) -> &str {
        self.tool
    }
    /// Whether writes through this scheme are allowed.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.writable
    }
}

/// Return every build-validated tool-family policy in stable family order.
#[must_use]
pub const fn tool_bundle_presets() -> &'static [BaseToolsPreset] {
    TOOL_BUNDLE_PRESETS
}

/// Return the foundational `hya/base-tools` policy for compatibility.
#[must_use]
pub fn base_tools_preset() -> &'static BaseToolsPreset {
    &TOOL_BUNDLE_PRESETS[0]
}
