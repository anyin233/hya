//! One resolution seam over two agent origins.
//!
//! Built-in agents are Rust constants ([`crate::builtin_agents`]) on the full
//! Harness tool plane. Installed bundle agents come from a
//! [`BundleCatalog`] and run on the clamped internal-public plane. Call sites
//! must not branch on the difference: they resolve through [`AgentCatalog`] and
//! receive an origin-tagged [`AgentDefinition`].
//!
//! **Origin decides the tool plane.** No manifest field selects it.

use std::borrow::Cow;
use std::sync::Arc;

use hya_bundle::{AgentRole, BundleCatalog, BundleError, ModelPolicy};

use crate::builtin_agents::{CoreAgentsPreset, SpawnScope, builtin_digest, core_agents_preset};

/// Where a resolved agent came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentOrigin<'a> {
    /// Compiled-in agent. Full Harness plane, owns no bundle resources.
    Builtin,
    /// Installed AgentBundle agent. Clamped plane plus its own bundle resources.
    Bundle {
        /// Identity id of the owning bundle.
        bundle_id: &'a str,
    },
}

impl AgentOrigin<'_> {
    /// Whether this agent is compiled in.
    #[must_use]
    pub const fn is_builtin(&self) -> bool {
        matches!(self, Self::Builtin)
    }

    /// Whether this definition comes from a trusted immutable preset.
    #[must_use]
    pub const fn is_preset(&self) -> bool {
        matches!(self, Self::Builtin)
    }

    /// Trusted preset identity, when this is a built-in definition.
    #[must_use]
    pub const fn preset_bundle_id(&self) -> Option<&'static str> {
        match self {
            Self::Builtin => Some(crate::builtin_agents::CORE_AGENTS_PRESET_ID),
            Self::Bundle { .. } => None,
        }
    }

    /// Owning bundle id, or `None` for a built-in.
    #[must_use]
    pub const fn bundle_id(&self) -> Option<&str> {
        match self {
            Self::Builtin => None,
            Self::Bundle { bundle_id } => Some(bundle_id),
        }
    }
}

/// Borrowed, origin-tagged view of one agent, whatever its origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDefinition<'a> {
    /// Stable agent id used for selection, spawn graphs, and session binding.
    pub stable_id: &'a str,
    /// Optional human description for selectors.
    pub description: Option<&'a str>,
    /// Main vs subagent selector role.
    pub role: AgentRole,
    /// Optional UI color hint.
    pub color: Option<&'a str>,
    /// Prompt body when the agent replaces the Harness base prompt.
    pub prompt: Option<&'a str>,
    /// Model/category/reasoning overrides.
    pub model_policy: Cow<'a, ModelPolicy>,
    /// Optional workdir override for turns of this agent.
    pub workdir: Option<&'a str>,
    /// Which plane and resource set this agent binds to.
    pub origin: AgentOrigin<'a>,
}

impl AgentDefinition<'_> {
    /// Compat/TUI selector mode for this agent's role.
    #[must_use]
    pub const fn selector_mode(&self) -> &'static str {
        self.role.selector_mode()
    }
}

/// Built-ins plus installed bundles, resolved as one namespace.
#[derive(Debug)]
pub struct AgentCatalog {
    preset: CoreAgentsPreset,
    bundles: Arc<BundleCatalog>,
}

impl AgentCatalog {
    /// Join the compiled-in roster with an installed-bundle catalog.
    ///
    /// # Errors
    /// Returns [`BundleError::BuiltinAgentIdShadowed`] when an installed bundle
    /// declares an agent id that a built-in already owns.
    pub fn new(bundles: Arc<BundleCatalog>) -> Result<Self, BundleError> {
        let catalog = Self {
            preset: core_agents_preset()?,
            bundles,
        };
        catalog.validate()?;
        Ok(catalog)
    }

    /// Reject installed bundles that shadow a built-in agent id.
    fn validate(&self) -> Result<(), BundleError> {
        for bundle in self.bundles.bundles() {
            for agent in bundle.agents() {
                if self.preset_agent(agent.id.as_str()).is_some() {
                    return Err(BundleError::BuiltinAgentIdShadowed {
                        bundle_id: bundle.identity().id.clone(),
                        agent_id: agent.id.as_str().to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Installed-bundle catalog backing this agent catalog.
    #[must_use]
    pub fn bundles(&self) -> &Arc<BundleCatalog> {
        &self.bundles
    }

    /// Digest of the compiled-in roster, for the runtime semantic fingerprint.
    #[must_use]
    pub fn builtin_digest(&self) -> &'static [u8; 32] {
        builtin_digest()
    }

    /// Domain-separated semantic identity of the whole agent surface.
    ///
    /// Composes the compiled-in roster digest with the installed-bundle
    /// catalog's own identity. Returns `None` when installed bundles are
    /// present but carry no verified provenance, which preserves the rule that
    /// an unverified catalog has no identity.
    #[must_use]
    pub fn semantic_identity_v1(&self) -> Option<Vec<u8>> {
        let bundle_identity = match self.bundles.semantic_identity_v1() {
            Some(identity) => identity.to_vec(),
            // A catalog with zero installed bundles has nothing to attest, so an
            // empty section is correct rather than "unidentifiable".
            None if self.bundles.bundles().is_empty() => Vec::new(),
            None => return None,
        };
        let mut identity = Vec::new();
        append_section(&mut identity, AGENT_CATALOG_IDENTITY_DOMAIN_V1);
        append_section(&mut identity, builtin_digest());
        append_section(&mut identity, &bundle_identity);
        Some(identity)
    }

    /// Resolve by stable id, or by a `bundle:{id}/agent/{local}` reference.
    ///
    /// Built-ins win on an exact id match. [`Self::validate`] has already
    /// rejected any bundle that could make that ambiguous.
    #[must_use]
    pub fn resolve(&self, reference: &str) -> Option<AgentDefinition<'_>> {
        if let Some(agent) = self.preset_agent(reference) {
            return Some(preset_definition(agent));
        }
        let (bundle_id, agent) = self.bundles.resolve_agent_entry(reference)?;
        Some(AgentDefinition {
            stable_id: agent.id.as_str(),
            description: agent.description.as_deref(),
            role: agent.role,
            color: agent.color.as_deref(),
            prompt: agent.prompt.as_deref(),
            model_policy: Cow::Borrowed(&agent.model_policy),
            workdir: agent.workdir.as_deref(),
            origin: AgentOrigin::Bundle { bundle_id },
        })
    }

    /// Resolve or fail with [`BundleError::UnknownAgentId`].
    pub fn require(&self, reference: &str) -> Result<AgentDefinition<'_>, BundleError> {
        self.resolve(reference)
            .ok_or_else(|| BundleError::UnknownAgentId {
                agent_id: reference.to_string(),
            })
    }

    /// Agents `caller` may spawn, for roster display.
    ///
    /// A `can_spawn` entry naming an agent that is not installed is **skipped**
    /// here. Bundles install independently, so a missing target must not make
    /// the caller unusable. [`Self::resolve_spawn`] still refuses it, so the
    /// leniency is display-only and never authorises a spawn.
    ///
    /// # Errors
    /// Returns [`BundleError::UnknownAgentId`] when `caller` itself is unknown.
    pub fn spawnable(&self, caller: &str) -> Result<Vec<AgentDefinition<'_>>, BundleError> {
        match self.spawn_scope_of(caller)? {
            CallerScope::Builtin(SpawnScope::None) => Ok(Vec::new()),
            CallerScope::Builtin(SpawnScope::AllOrdinary) => Ok(self.all_ordinary()),
            CallerScope::Bundle(can_spawn) => Ok(can_spawn
                .iter()
                .filter_map(|target| self.resolve(target.as_str()))
                .collect()),
        }
    }

    /// Authorise one spawn. Strict: an unknown or unlisted target is an error.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let scope = self.spawn_scope_of(caller)?;
        let target = self.require(requested)?;
        let allowed = match scope {
            CallerScope::Builtin(SpawnScope::None) => false,
            CallerScope::Builtin(SpawnScope::AllOrdinary) => !self.is_reserved(target.stable_id),
            CallerScope::Bundle(can_spawn) => can_spawn
                .iter()
                .any(|listed| listed.as_str() == target.stable_id),
        };
        if !allowed {
            return Err(BundleError::AgentSpawnNotAllowed {
                caller: caller.to_string(),
                agent_id: target.stable_id.to_string(),
            });
        }
        Ok(target)
    }

    /// Every selectable agent, built-in and installed, for selector listings.
    #[must_use]
    pub fn selectable(&self) -> Vec<AgentDefinition<'_>> {
        self.all_ordinary()
    }

    /// Every agent in the catalog, including the reserved system agents.
    ///
    /// Reserved agents are never selectable or spawnable; listings that must
    /// still show them (the Compat agent metadata surface marks them `hidden`)
    /// use this instead of [`Self::selectable`].
    #[must_use]
    pub fn all(&self) -> Vec<AgentDefinition<'_>> {
        let mut agents = self.all_ordinary();
        for agent in self
            .preset
            .agents()
            .iter()
            .filter(|agent| self.preset.is_reserved(agent.id.as_str()))
        {
            agents.push(preset_definition(agent));
        }
        agents.sort_by(|left, right| left.stable_id.cmp(right.stable_id));
        agents
    }

    /// Whether `id` is a reserved system agent that no ordinary agent may spawn.
    #[must_use]
    pub fn is_reserved(&self, id: &str) -> bool {
        self.preset.is_reserved(id)
    }

    /// Non-reserved built-ins followed by every installed bundle agent.
    fn all_ordinary(&self) -> Vec<AgentDefinition<'_>> {
        let mut agents = self
            .preset
            .agents()
            .iter()
            .filter(|agent| !self.preset.is_reserved(agent.id.as_str()))
            .map(preset_definition)
            .collect::<Vec<_>>();
        for bundle in self.bundles.bundles() {
            for agent in bundle.agents() {
                if let Some(definition) = self.resolve(agent.id.as_str()) {
                    agents.push(definition);
                }
            }
        }
        agents.sort_by(|left, right| left.stable_id.cmp(right.stable_id));
        agents
    }

    fn spawn_scope_of(&self, caller: &str) -> Result<CallerScope<'_>, BundleError> {
        if let Some(agent) = self.preset_agent(caller) {
            let scope = if self.preset.is_reserved(agent.id.as_str()) {
                SpawnScope::None
            } else {
                SpawnScope::AllOrdinary
            };
            return Ok(CallerScope::Builtin(scope));
        }
        let (_, agent) = self.bundles.resolve_agent_entry(caller).ok_or_else(|| {
            BundleError::UnknownAgentId {
                agent_id: caller.to_string(),
            }
        })?;
        Ok(CallerScope::Bundle(&agent.can_spawn))
    }

    fn preset_agent(&self, reference: &str) -> Option<&hya_bundle::PreparedAgent> {
        let id = reference
            .strip_prefix("bundle:hya/core-agents/agent/")
            .unwrap_or(reference);
        self.preset
            .agents()
            .binary_search_by(|agent| agent.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.preset.agents()[index])
    }
}

/// Domain separator for [`AgentCatalog::semantic_identity_v1`].
const AGENT_CATALOG_IDENTITY_DOMAIN_V1: &[u8] = b"hya.core.agent-catalog.semantic-identity/v1";

/// Append one length-prefixed section, so concatenation stays unambiguous.
fn append_section(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value);
}

/// What the caller is allowed to spawn, normalised across origins.
enum CallerScope<'a> {
    Builtin(SpawnScope),
    Bundle(&'a [hya_proto::AgentName]),
}

fn preset_definition(agent: &hya_bundle::PreparedAgent) -> AgentDefinition<'_> {
    AgentDefinition {
        stable_id: agent.id.as_str(),
        description: agent.description.as_deref(),
        role: agent.role,
        color: agent.color.as_deref(),
        prompt: agent.prompt.as_deref(),
        model_policy: Cow::Borrowed(&agent.model_policy),
        workdir: agent.workdir.as_deref(),
        origin: AgentOrigin::Builtin,
    }
}
