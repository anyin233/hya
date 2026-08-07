//! Compiled-in agent definitions.
//!
//! Built-in agents are **not** AgentBundles. They ship with the binary as Rust
//! constants, they own no bundle resources, and they run on the full Harness
//! tool plane. Installed AgentBundles are the separate, clamped surface — see
//! [`crate::agent_catalog`] for the union both are resolved through.
//!
//! Editing a prompt under `prompts/` requires a rebuild, exactly as the retired
//! `bundles/builtin/` prepare step did.

use std::borrow::Cow;
use std::sync::OnceLock;

use hya_bundle::{AgentRole, ModelPolicy, SpawnLifecycle};
use sha2::{Digest, Sha256};

use crate::agent_catalog::{AgentDefinition, AgentOrigin};

/// Domain separator for the built-in roster digest.
const BUILTIN_DIGEST_DOMAIN_V1: &[u8] = b"hya.core.builtin-agents/v1";

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

/// Shorthand for an ordinary (non-reserved) subagent with a compiled-in prompt.
const fn ordinary_subagent(
    id: &'static str,
    description: &'static str,
    prompt: Option<&'static str>,
) -> BuiltinAgent {
    BuiltinAgent {
        id,
        description: Some(description),
        role: AgentRole::Subagent,
        prompt,
        model_policy: BuiltinModelPolicy::DEFAULT,
        spawn_lifecycle: SpawnLifecycle::Transient,
        spawn_scope: SpawnScope::AllOrdinary,
        system_reserved: false,
    }
}

/// Shorthand for an ordinary primary agent.
const fn ordinary_main(
    id: &'static str,
    description: &'static str,
    prompt: Option<&'static str>,
) -> BuiltinAgent {
    BuiltinAgent {
        id,
        description: Some(description),
        role: AgentRole::Main,
        prompt,
        model_policy: BuiltinModelPolicy::DEFAULT,
        spawn_lifecycle: SpawnLifecycle::Transient,
        spawn_scope: SpawnScope::AllOrdinary,
        system_reserved: false,
    }
}

/// Shorthand for a reserved system agent: unselectable and unspawnable.
const fn system_agent(id: &'static str, prompt: &'static str) -> BuiltinAgent {
    BuiltinAgent {
        id,
        description: None,
        role: AgentRole::Subagent,
        prompt: Some(prompt),
        model_policy: BuiltinModelPolicy::DEFAULT,
        spawn_lifecycle: SpawnLifecycle::Transient,
        spawn_scope: SpawnScope::None,
        system_reserved: true,
    }
}

/// The complete built-in roster, strictly sorted by id.
///
/// Sort order is load-bearing: it makes lookup a binary search and makes
/// [`builtin_digest`] stable across builds.
pub const BUILTIN_AGENTS: &[BuiltinAgent] = &[
    ordinary_main(
        "build",
        "The default agent. Executes tools based on configured permissions.",
        None,
    ),
    system_agent("compaction", include_str!("prompts/compaction.md")),
    ordinary_subagent(
        "explore",
        concat!(
            "Fast agent specialized for exploring codebases. Use this when you need to quickly ",
            "find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords ",
            "(eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API ",
            "endpoints work?\"). When calling this agent, specify the desired thoroughness level: ",
            "\"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" ",
            "for comprehensive analysis across multiple locations and naming conventions."
        ),
        Some(include_str!("prompts/explore.md")),
    ),
    ordinary_subagent(
        "general",
        concat!(
            "General-purpose agent for researching complex questions and executing multi-step ",
            "tasks. Use this agent to execute multiple units of work in parallel."
        ),
        None,
    ),
    ordinary_subagent(
        "hya-docs",
        "Transient subagent for requested documentation, API docs, glossary, and ADR updates.",
        Some(include_str!("prompts/hya-docs.md")),
    ),
    ordinary_subagent(
        "hya-explorer",
        "Transient subagent for codebase reconnaissance, flows, conventions, and blast radius.",
        Some(include_str!("prompts/hya-explorer.md")),
    ),
    ordinary_subagent(
        "hya-implementer",
        "Transient subagent for focused code changes after scope and target files are clear.",
        Some(include_str!("prompts/hya-implementer.md")),
    ),
    ordinary_main(
        "hya-main",
        concat!(
            "Default primary agent for coding work. Delegates to specialist subagents and ",
            "integrates verified results."
        ),
        Some(include_str!("prompts/hya-main.md")),
    ),
    ordinary_subagent(
        "hya-planner",
        "Transient subagent for design tradeoffs, implementation plans, and task breakdowns.",
        Some(include_str!("prompts/hya-planner.md")),
    ),
    ordinary_subagent(
        "hya-release",
        "Transient subagent for version, changelog, tag, and release readiness work.",
        Some(include_str!("prompts/hya-release.md")),
    ),
    ordinary_subagent(
        "hya-reviewer",
        "Transient subagent for correctness, standards, security, and simplification review.",
        Some(include_str!("prompts/hya-reviewer.md")),
    ),
    ordinary_subagent(
        "hya-tester",
        "Transient subagent for TDD tests, behavioral coverage, and focused verification.",
        Some(include_str!("prompts/hya-tester.md")),
    ),
    ordinary_main(
        "plan",
        "Plan mode. Planning-focused agent for designs and task breakdowns.",
        None,
    ),
    system_agent("summary", include_str!("prompts/summary.md")),
    system_agent("title", include_str!("prompts/title.md")),
];

/// Resolve a built-in by exact id.
#[must_use]
pub fn builtin_agent(id: &str) -> Option<&'static BuiltinAgent> {
    BUILTIN_AGENTS
        .binary_search_by(|agent| agent.id.cmp(id))
        .ok()
        .map(|index| &BUILTIN_AGENTS[index])
}

/// Every ordinary (non-reserved) built-in, in roster order.
#[must_use]
pub fn ordinary_builtins() -> impl Iterator<Item = &'static BuiltinAgent> {
    BUILTIN_AGENTS.iter().filter(|agent| !agent.system_reserved)
}

/// Whether `id` names a built-in agent that installed bundles must not shadow.
#[must_use]
pub fn is_builtin_id(id: &str) -> bool {
    builtin_agent(id).is_some()
}

/// SHA-256 over the canonical serialisation of [`BUILTIN_AGENTS`].
///
/// Folded into the runtime semantic fingerprint in place of the digests the
/// retired built-in bundles used to contribute. Constant per binary, so a
/// rebuild with an edited prompt changes the fingerprint.
#[must_use]
pub fn builtin_digest() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update((BUILTIN_DIGEST_DOMAIN_V1.len() as u64).to_be_bytes());
        hasher.update(BUILTIN_DIGEST_DOMAIN_V1);
        hasher.update((BUILTIN_AGENTS.len() as u64).to_be_bytes());
        for agent in BUILTIN_AGENTS {
            for field in [
                agent.id,
                agent.description.unwrap_or_default(),
                agent.role.selector_mode(),
                agent.prompt.unwrap_or_default(),
                agent.model_policy.model.unwrap_or_default(),
                agent.model_policy.category.unwrap_or_default(),
                agent.model_policy.reasoning.unwrap_or_default(),
            ] {
                hasher.update((field.len() as u64).to_be_bytes());
                hasher.update(field.as_bytes());
            }
            hasher.update([
                u8::from(agent.spawn_lifecycle == SpawnLifecycle::Resident),
                u8::from(agent.spawn_scope == SpawnScope::AllOrdinary),
                u8::from(agent.system_reserved),
                // Presence flags: an empty prompt and an absent prompt must not
                // hash the same.
                u8::from(agent.description.is_some()),
                u8::from(agent.prompt.is_some()),
            ]);
        }
        hasher.finalize().into()
    })
}
