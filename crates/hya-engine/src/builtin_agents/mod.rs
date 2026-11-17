//! Built-in agent definitions from the trusted `hya/core-agents` bundle.
//!
//! The AgentSetBundle is loaded from its first-party source or package when the
//! process first needs it; nothing is compiled into the binary. Every lookup
//! reads the verified prepared preset.

use std::sync::OnceLock;

use hya_bundle::{Bundle, BundleError, PreparedAgent, PreparedCatalog, first_party_bundle};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Stable identity of the trusted core-agents preset.
pub const CORE_AGENTS_PRESET_ID: &str = "hya/core-agents";

/// Default primary agent: new root sessions run it unless configuration or the
/// request selects another.
pub const MAIN_AGENT_ID: &str = "hya-main";

/// General-purpose worker: the `task` target when `subagent_type` is empty, and
/// the agent a stored session continues as when its recorded agent no longer
/// exists in the catalog.
pub const TASK_AGENT_ID: &str = "hya-task";

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

/// Verified core-agents preset with its parsed reserved-id policy.
struct LoadedCoreAgents {
    catalog: &'static PreparedCatalog,
    bundle: &'static Bundle,
    reserved_ids: Vec<&'static str>,
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
        .ok_or_else(|| invalid("preset contains no Bundle".to_string()))?;
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
    Ok(LoadedCoreAgents {
        catalog,
        bundle,
        reserved_ids,
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

/// Prepared agents of the verified core-agents preset, in stable-id order.
///
/// # Panics
///
/// Panics when the trusted core-agents bundle is missing or invalid.
#[must_use]
pub fn preset_agents() -> &'static [PreparedAgent] {
    &required_core_agents().bundle.agents
}

/// Prepared agent from the verified core-agents preset, by exact id.
#[must_use]
pub fn preset_agent(id: &str) -> Option<&'static PreparedAgent> {
    let agents = preset_agents();
    agents
        .binary_search_by(|agent| agent.id.as_str().cmp(id))
        .ok()
        .map(|index| &agents[index])
}

/// Every built-in agent id, sorted: an install refuses a bundle that claims one.
#[must_use]
pub fn builtin_agent_ids() -> Vec<&'static str> {
    preset_agents()
        .iter()
        .map(|agent| agent.id.as_str())
        .collect()
}

/// SHA-256 digest bytes of the loaded prepared preset.
#[must_use]
pub fn builtin_digest() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| Sha256::digest(required_core_agents().catalog.bytes()).into())
}
