//! Built-in agent definitions from the trusted `hya/core-agents` bundle.
//!
//! The AgentSetBundle is loaded from its first-party source or package when the
//! process first needs it; nothing is compiled into the binary. The public
//! `BuiltinAgent` roster is a compatibility view of that prepared preset; new
//! catalog assembly reads the verified preset itself.

use std::borrow::Cow;
use std::sync::OnceLock;

use hya_bundle::{AgentRole, ModelPolicy};
use hya_bundle::{
    BundleError, PreparedAgent, PreparedAgentSetBundle, PreparedCatalog, PreparedInstallableBundle,
    first_party_bundle,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::agent_catalog::{AgentDefinition, AgentOrigin};

/// Stable identity of the trusted core-agents preset.
pub const CORE_AGENTS_PRESET_ID: &str = "hya/core-agents";

/// The preset's `extensions.files` policy asset.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreAgentsPolicy {
    reserved_ids: Vec<String>,
    ordinary_spawn_scope: String,
}

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

/// One built-in agent from the core-agents preset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinAgent {
    /// Stable agent id. Also the selector name and the spawn-graph key.
    pub id: &'static str,
    /// Human description for selectors. `None` only for reserved system agents.
    pub description: Option<&'static str>,
    /// Main vs subagent selector role.
    pub role: AgentRole,
    /// Preset prompt body, or `None` to keep the Harness base prompt.
    pub prompt: Option<&'static str>,
    /// Model/category/reasoning overrides.
    pub model_policy: BuiltinModelPolicy,
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
            origin: AgentOrigin::Builtin,
        }
    }
}

/// Verified core-agents preset with its parsed policy and compatibility roster.
struct LoadedCoreAgents {
    catalog: &'static PreparedCatalog,
    bundle: &'static PreparedAgentSetBundle,
    reserved_ids: Vec<&'static str>,
    roster: Vec<BuiltinAgent>,
}

fn load_core_agents() -> Result<LoadedCoreAgents, BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: CORE_AGENTS_PRESET_ID.to_string(),
        detail,
    };
    let catalog = first_party_bundle(CORE_AGENTS_PRESET_ID)?;
    let bundle = catalog
        .bundles()
        .first()
        .and_then(PreparedInstallableBundle::agent_set_bundle)
        .ok_or_else(|| invalid("preset is not an AgentSetBundle".to_string()))?;
    let policy = bundle
        .extensions
        .iter()
        .find(|resource| resource.local_id == "policy")
        .ok_or_else(|| invalid("preset must contain extensions.files policy".to_string()))?;
    let policy: CoreAgentsPolicy = serde_norway::from_str(&policy.content)
        .map_err(|error| invalid(format!("invalid core-agents policy: {error}")))?;
    if policy.ordinary_spawn_scope != "all_ordinary" {
        return Err(invalid(
            "core-agents ordinary_spawn_scope must be all_ordinary".to_string(),
        ));
    }
    let mut reserved_ids = Vec::with_capacity(policy.reserved_ids.len());
    for reserved in &policy.reserved_ids {
        let agent = bundle
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == reserved)
            .ok_or_else(|| invalid(format!("policy reserves unknown agent `{reserved}`")))?;
        reserved_ids.push(agent.id.as_str());
    }
    reserved_ids.sort_unstable();
    reserved_ids.dedup();
    let roster = bundle
        .agents
        .iter()
        .map(|agent| {
            let reserved = reserved_ids.binary_search(&agent.id.as_str()).is_ok();
            BuiltinAgent {
                id: agent.id.as_str(),
                description: agent.description.as_deref(),
                role: agent.role,
                prompt: agent.prompt.as_deref(),
                model_policy: BuiltinModelPolicy {
                    model: agent.model_policy.model.as_deref(),
                    category: agent.model_policy.category.as_deref(),
                    reasoning: agent.model_policy.reasoning.as_deref(),
                },
                spawn_scope: if reserved {
                    SpawnScope::None
                } else {
                    SpawnScope::AllOrdinary
                },
                system_reserved: reserved,
            }
        })
        .collect();
    Ok(LoadedCoreAgents {
        catalog,
        bundle,
        reserved_ids,
        roster,
    })
}

fn loaded_core_agents() -> Result<&'static LoadedCoreAgents, BundleError> {
    static LOADED: OnceLock<Result<LoadedCoreAgents, BundleError>> = OnceLock::new();
    LOADED
        .get_or_init(load_core_agents)
        .as_ref()
        .map_err(Clone::clone)
}

fn required_core_agents() -> &'static LoadedCoreAgents {
    loaded_core_agents().unwrap_or_else(|error| panic!("load {CORE_AGENTS_PRESET_ID}: {error}"))
}

/// Built-in agents in stable-id order.
///
/// # Panics
///
/// Panics when the trusted core-agents bundle is missing or invalid.
#[must_use]
pub fn builtin_agents() -> &'static [BuiltinAgent] {
    &required_core_agents().roster
}

/// Borrowed view of the verified core-agents preset.
#[derive(Clone, Copy)]
pub struct CoreAgentsPreset {
    loaded: &'static LoadedCoreAgents,
}

impl std::fmt::Debug for CoreAgentsPreset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreAgentsPreset")
            .field("bundle_id", &self.bundle_id())
            .field("digest", &self.digest())
            .finish()
    }
}

impl CoreAgentsPreset {
    /// Stable preset bundle id.
    #[must_use]
    pub fn bundle_id(self) -> &'static str {
        self.loaded.bundle.identity.id.as_str()
    }

    /// Preset version from the verified prepared identity.
    #[must_use]
    pub fn version(self) -> &'static str {
        self.loaded.bundle.identity.version.as_str()
    }

    /// Canonical prepared document bytes of the loaded bundle.
    #[must_use]
    pub fn prepared_bytes(self) -> &'static [u8] {
        self.loaded.catalog.bytes()
    }

    /// SHA-256 hex digest of the canonical prepared document.
    #[must_use]
    pub fn digest(self) -> &'static str {
        self.loaded.catalog.digest()
    }

    /// Prepared agents in stable-id order.
    #[must_use]
    pub fn agents(self) -> &'static [PreparedAgent] {
        &self.loaded.bundle.agents
    }

    /// Whether an id belongs to an engine-only system agent in this preset.
    #[must_use]
    pub fn is_reserved(self, id: &str) -> bool {
        self.loaded.reserved_ids.binary_search(&id).is_ok()
    }
}

/// Load and verify the trusted core-agents preset once per process.
///
/// # Errors
///
/// Returns the first-party load failure or an invalid preset policy.
pub fn core_agents_preset() -> Result<CoreAgentsPreset, BundleError> {
    loaded_core_agents().map(|loaded| CoreAgentsPreset { loaded })
}

/// Resolve a built-in by exact id.
#[must_use]
pub fn builtin_agent(id: &str) -> Option<&'static BuiltinAgent> {
    let roster = builtin_agents();
    roster
        .binary_search_by(|agent| agent.id.cmp(id))
        .ok()
        .map(|index| &roster[index])
}

/// Every ordinary (non-reserved) built-in, in roster order.
pub fn ordinary_builtins() -> impl Iterator<Item = &'static BuiltinAgent> {
    builtin_agents()
        .iter()
        .filter(|agent| !agent.system_reserved)
}

/// Whether `id` names a built-in agent that installed bundles must not shadow.
#[must_use]
pub fn is_builtin_id(id: &str) -> bool {
    builtin_agent(id).is_some()
}

/// SHA-256 digest bytes of the loaded prepared preset.
#[must_use]
pub fn builtin_digest() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| Sha256::digest(required_core_agents().catalog.bytes()).into())
}
