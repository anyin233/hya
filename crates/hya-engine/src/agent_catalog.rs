//! One resolution seam over two agent origins.
//!
//! The compiled-in `hya-bare` ([`crate::builtin_agents`]) is part of the
//! roster only when no enabled bundle declares `default_agent`; every other
//! agent comes from a [`BundleCatalog`]. Call sites must not branch on the
//! difference: they resolve through [`AgentCatalog`] and receive an
//! origin-tagged [`AgentDefinition`].
//!
//! **Origin decides the tool plane.** No manifest field selects it.

use std::borrow::Cow;
use std::sync::Arc;

use hya_bundle::{AgentRole, BundleCatalog, BundleError, ModelPolicy, SPAWN_ANY_AGENT};

use crate::builtin_agents::{BARE_AGENT_ID, core_agent, core_agent_digest};

/// Where a resolved agent came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentOrigin<'a> {
    /// The compiled-in `hya-bare`. Full Harness plane, owns no bundle resources.
    Builtin,
    /// Bundle agent. Its bundle's plane plus its own bundle resources.
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
    /// Whether `can_spawn: ["*"]` grants and selector listings exclude this agent.
    pub private: bool,
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

/// The enabled bundles' agents, plus `hya-bare` when no bundle declares
/// `default_agent`, resolved as one namespace.
#[derive(Debug)]
pub struct AgentCatalog {
    bundles: Arc<BundleCatalog>,
}

impl AgentCatalog {
    /// Build the roster of `bundles`.
    ///
    /// # Errors
    /// Returns [`BundleError::BuiltinAgentIdShadowed`] when a bundle declares
    /// the compiled-in `hya-bare` id.
    pub fn new(bundles: Arc<BundleCatalog>) -> Result<Self, BundleError> {
        for bundle in bundles.bundles() {
            if let Some(agent) = bundle
                .agents()
                .iter()
                .find(|agent| agent.id.as_str() == BARE_AGENT_ID)
            {
                return Err(BundleError::BuiltinAgentIdShadowed {
                    bundle_id: bundle.identity().id.clone(),
                    agent_id: agent.id.as_str().to_string(),
                });
            }
        }
        Ok(Self { bundles })
    }

    /// Default primary agent id: the `default_agent` an enabled bundle
    /// declares (at most one may, [`BundleCatalog`] enforces it), else
    /// `hya-bare`.
    #[must_use]
    pub fn default_agent_id(&self) -> &str {
        self.declared_default().unwrap_or(BARE_AGENT_ID)
    }

    fn declared_default(&self) -> Option<&str> {
        self.bundles
            .bundles()
            .iter()
            .find_map(|bundle| bundle.default_agent())
            .map(hya_proto::AgentName::as_str)
    }

    /// Whether the compiled-in `hya-bare` is part of this roster.
    fn has_core_agent(&self) -> bool {
        self.declared_default().is_none()
    }

    /// Bundle catalog backing this agent catalog.
    #[must_use]
    pub fn bundles(&self) -> &Arc<BundleCatalog> {
        &self.bundles
    }

    /// Domain-separated semantic identity of the whole agent surface.
    ///
    /// Composes the `hya-bare` digest (an empty section when `hya-bare` is not
    /// in the roster) with the bundle catalog's own identity. Returns `None`
    /// when bundles are present but carry no verified provenance, which
    /// preserves the rule that an unverified catalog has no identity.
    #[must_use]
    pub fn semantic_identity_v1(&self) -> Option<Vec<u8>> {
        let bundle_identity = match self.bundles.semantic_identity_v1() {
            Some(identity) => identity.to_vec(),
            // A catalog with zero bundles has nothing to attest, so an empty
            // section is correct rather than "unidentifiable".
            None if self.bundles.bundles().is_empty() => Vec::new(),
            None => return None,
        };
        let core_agent: &[u8] = if self.has_core_agent() {
            core_agent_digest()
        } else {
            &[]
        };
        let mut identity = Vec::new();
        append_section(&mut identity, AGENT_CATALOG_IDENTITY_DOMAIN_V1);
        append_section(&mut identity, core_agent);
        append_section(&mut identity, &bundle_identity);
        Some(identity)
    }

    /// Resolve by stable id, or by a `bundle:{id}/agent/{local}` reference.
    #[must_use]
    pub fn resolve(&self, reference: &str) -> Option<AgentDefinition<'_>> {
        if reference == BARE_AGENT_ID && self.has_core_agent() {
            return Some(definition(core_agent(), AgentOrigin::Builtin));
        }
        let (bundle_id, agent) = self.bundles.resolve_agent_entry(reference)?;
        Some(definition(agent, AgentOrigin::Bundle { bundle_id }))
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
            CallerScope::All => Ok(self.all_ordinary()),
            CallerScope::Listed(can_spawn) => Ok(can_spawn
                .iter()
                .filter_map(|target| self.resolve(target.as_str()))
                .collect()),
        }
    }

    /// Authorise one spawn. Strict: an unknown or unlisted target is an error,
    /// and an all-agents grant never reaches a private agent.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let target = self.resolve_agent_run(caller, requested)?;
        if target.private && matches!(self.spawn_scope_of(caller)?, CallerScope::All) {
            return Err(BundleError::AgentSpawnNotAllowed {
                caller: caller.to_string(),
                agent_id: target.stable_id.to_string(),
            });
        }
        Ok(target)
    }

    /// Authorise a directed agent run, including private agents.
    /// An all-agents caller may run any target; other callers need an
    /// explicit `can_spawn` grant.
    pub fn resolve_agent_run(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let scope = self.spawn_scope_of(caller)?;
        let target = self.require(requested)?;
        let allowed = match scope {
            CallerScope::All => true,
            CallerScope::Listed(can_spawn) => can_spawn
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

    /// Every non-private agent, for selector listings.
    #[must_use]
    pub fn selectable(&self) -> Vec<AgentDefinition<'_>> {
        self.all_ordinary()
    }

    /// Every agent, including private ones. Metadata surfaces use this list
    /// and mark private entries hidden.
    #[must_use]
    pub fn all(&self) -> Vec<AgentDefinition<'_>> {
        let mut agents = Vec::new();
        if self.has_core_agent() {
            agents.push(definition(core_agent(), AgentOrigin::Builtin));
        }
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

    fn all_ordinary(&self) -> Vec<AgentDefinition<'_>> {
        self.all()
            .into_iter()
            .filter(|agent| !agent.private)
            .collect()
    }

    fn spawn_scope_of(&self, caller: &str) -> Result<CallerScope<'_>, BundleError> {
        if caller == BARE_AGENT_ID && self.has_core_agent() {
            return Ok(CallerScope::All);
        }
        let (_, agent) = self.bundles.resolve_agent_entry(caller).ok_or_else(|| {
            BundleError::UnknownAgentId {
                agent_id: caller.to_string(),
            }
        })?;
        Ok(match agent.can_spawn.as_slice() {
            [only] if only.as_str() == SPAWN_ANY_AGENT => CallerScope::All,
            listed => CallerScope::Listed(listed),
        })
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
    /// `hya-bare` and `can_spawn: ["*"]`: every agent (spawns: non-private only).
    All,
    Listed(&'a [hya_proto::AgentName]),
}

fn definition<'a>(
    agent: &'a hya_bundle::PreparedAgent,
    origin: AgentOrigin<'a>,
) -> AgentDefinition<'a> {
    AgentDefinition {
        stable_id: agent.id.as_str(),
        private: agent.private,
        description: agent.description.as_deref(),
        role: agent.role,
        color: agent.color.as_deref(),
        prompt: agent.prompt.as_deref(),
        model_policy: Cow::Borrowed(&agent.model_policy),
        workdir: agent.workdir.as_deref(),
        origin,
    }
}
