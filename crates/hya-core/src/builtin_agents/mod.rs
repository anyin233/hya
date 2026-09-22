//! Compiled-in agent definitions.
//!
//! The trusted `hya/core-agents` AgentSetBundle is prepared by `build.rs`, then
//! its canonical bytes and digest are embedded in the binary. The public
//! `BuiltinAgent` roster is a compatibility view generated from that prepared
//! preset; new catalog assembly reads the verified preset itself.

use std::borrow::Cow;
use std::sync::OnceLock;

use hya_bundle::{AgentRole, ModelPolicy, SpawnLifecycle};
use hya_bundle::{
    BundleError, PreparedAgent, PreparedAgentSetBundle, PreparedCatalog, PreparedInstallableBundle,
};
use sha2::{Digest, Sha256};

use crate::agent_catalog::{AgentDefinition, AgentOrigin};

/// Stable identity of the trusted embedded preset.
pub const CORE_AGENTS_PRESET_ID: &str = "hya/core-agents";

const CORE_AGENTS_PREPARED_BYTES: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/core-agents.prepared.json"));
const CORE_AGENTS_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/core-agents.digest"));

/// Which agents a built-in may spawn.
///
/// Built-ins carry a *scope*, not a fixed id list, so installing an AgentBundle
/// makes its agent spawnable with no edit to any built-in definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnScope {
    /// Every non-reserved agent in the catalog: built-in or installed.
    AllOrdinary,
    /// Spawns nothing. Used by the reserved system agents.
    None,
}

/// Const-constructible model routing overrides.
///
/// [`ModelPolicy`] holds `Option<String>`, which cannot appear in a `const`.
/// This mirror uses `&'static str` and converts on demand.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BuiltinModelPolicy {
    /// Explicit model ref when set; otherwise the runtime default applies.
    pub model: Option<&'static str>,
    /// Optional model category hint for routing.
    pub category: Option<&'static str>,
    /// Optional reasoning effort string (provider-specific).
    pub reasoning: Option<&'static str>,
}

impl BuiltinModelPolicy {
    /// Policy with no overrides: the runtime default applies.
    pub const DEFAULT: Self = Self {
        model: None,
        category: None,
        reasoning: None,
    };

    /// Convert to the owned policy shape the runtime consumes.
    #[must_use]
    pub fn to_model_policy(self) -> ModelPolicy {
        ModelPolicy {
            model: self.model.map(str::to_string),
            category: self.category.map(str::to_string),
            reasoning: self.reasoning.map(str::to_string),
        }
    }
}

/// One compiled-in agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinAgent {
    /// Stable agent id. Also the selector name and the spawn-graph key.
    pub id: &'static str,
    /// Human description for selectors. `None` only for reserved system agents.
    pub description: Option<&'static str>,
    /// Main vs subagent selector role.
    pub role: AgentRole,
    /// Compiled-in prompt body, or `None` to keep the Harness base prompt.
    pub prompt: Option<&'static str>,
    /// Model/category/reasoning overrides.
    pub model_policy: BuiltinModelPolicy,
    /// Transient vs resident when Harness spawns this entry.
    pub spawn_lifecycle: SpawnLifecycle,
    /// What this agent may spawn.
    pub spawn_scope: SpawnScope,
    /// Reserved system agent: never selectable and never an ordinary spawn target.
    pub system_reserved: bool,
}

impl BuiltinAgent {
    /// Borrowed, origin-tagged view used by every catalog call site.
    #[must_use]
    pub fn definition(&'static self) -> AgentDefinition<'static> {
        AgentDefinition {
            stable_id: self.id,
            description: self.description,
            role: self.role,
            color: None,
            prompt: self.prompt,
            model_policy: Cow::Owned(self.model_policy.to_model_policy()),
            workdir: None,
            spawn_lifecycle: self.spawn_lifecycle,
            origin: AgentOrigin::Builtin,
        }
    }
}

// Generated from the prepared AgentSetBundle, retained for source compatibility.
include!(concat!(env!("OUT_DIR"), "/core_agents_roster.rs"));

/// Borrowed view of the verified embedded core-agents preset.
#[derive(Clone, Copy, Debug)]
pub struct CoreAgentsPreset {
    _catalog: &'static PreparedCatalog,
    bundle: &'static PreparedAgentSetBundle,
}

impl CoreAgentsPreset {
    /// Stable preset bundle id.
    #[must_use]
    pub fn bundle_id(self) -> &'static str {
        self.bundle.identity.id.as_str()
    }

    /// Preset version from the verified prepared identity.
    #[must_use]
    pub fn version(self) -> &'static str {
        self.bundle.identity.version.as_str()
    }

    /// Canonical prepared document bytes embedded in this binary.
    #[must_use]
    pub const fn prepared_bytes(self) -> &'static [u8] {
        CORE_AGENTS_PREPARED_BYTES
    }

    /// SHA-256 hex digest of the canonical prepared document.
    #[must_use]
    pub const fn digest(self) -> &'static str {
        CORE_AGENTS_DIGEST
    }

    /// Prepared agents in stable-id order.
    #[must_use]
    pub fn agents(self) -> &'static [PreparedAgent] {
        &self.bundle.agents
    }

    /// Whether an id belongs to an engine-only system agent in this preset.
    #[must_use]
    pub fn is_reserved(self, id: &str) -> bool {
        CORE_AGENT_RESERVED_IDS.binary_search(&id).is_ok()
    }
}

/// Decode and verify the build-time prepared preset once per process.
pub fn core_agents_preset() -> Result<CoreAgentsPreset, BundleError> {
    static PRESET: OnceLock<Result<PreparedCatalog, BundleError>> = OnceLock::new();
    let catalog = PRESET
        .get_or_init(|| PreparedCatalog::decode(CORE_AGENTS_PREPARED_BYTES, CORE_AGENTS_DIGEST))
        .as_ref()
        .map_err(Clone::clone)?;
    let bundle = catalog
        .bundles()
        .first()
        .and_then(PreparedInstallableBundle::agent_set_bundle)
        .ok_or_else(|| BundleError::InvalidManifest {
            source_name: CORE_AGENTS_PRESET_ID.to_string(),
            detail: "embedded preset is not an AgentSetBundle".to_string(),
        })?;
    Ok(CoreAgentsPreset {
        _catalog: catalog,
        bundle,
    })
}

/// Resolve a built-in by exact id.
#[must_use]
pub fn builtin_agent(id: &str) -> Option<&'static BuiltinAgent> {
    BUILTIN_AGENTS
        .binary_search_by(|agent| agent.id.cmp(id))
        .ok()
        .map(|index| &BUILTIN_AGENTS[index])
}

/// Every ordinary (non-reserved) built-in, in roster order.
pub fn ordinary_builtins() -> impl Iterator<Item = &'static BuiltinAgent> {
    BUILTIN_AGENTS.iter().filter(|agent| !agent.system_reserved)
}

/// Whether `id` names a built-in agent that installed bundles must not shadow.
#[must_use]
pub fn is_builtin_id(id: &str) -> bool {
    builtin_agent(id).is_some()
}

/// SHA-256 digest bytes of the embedded prepared preset.
#[must_use]
pub fn builtin_digest() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| Sha256::digest(CORE_AGENTS_PREPARED_BYTES).into())
}
