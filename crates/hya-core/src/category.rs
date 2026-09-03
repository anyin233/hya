use std::collections::HashMap;

use hya_bundle::ModelPolicy;
use hya_proto::ModelRef;

use crate::agent_catalog::AgentDefinition;
use crate::engine::AgentSpec;
use hya_tool::SpawnMember;

/// Apply the normal spawn model/category precedence to an already-resolved Agent.
///
/// The seven layers, from lowest to highest precedence, are the current base
/// model, Bundle category, inline category, Bundle model, inline model, spawn
/// category, and spawn model. Category resolution preserves first-match
/// servability behavior.
#[must_use]
pub fn apply_spawn_model_policy(
    mut agent: AgentSpec,
    definition: &AgentDefinition<'_>,
    member: &SpawnMember,
    categories: &CategoryRegistry,
    is_servable: &dyn Fn(&ModelRef) -> bool,
) -> AgentSpec {
    let resolve_category = |name: &str| {
        categories
            .resolve_servable(name, is_servable)
            .map(|resolved| resolved.model)
    };

    if let Some(model) = definition
        .model_policy
        .category
        .as_deref()
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.category.as_deref())
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = definition.model_policy.model.as_deref() {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.model.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .category
        .as_deref()
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }
    agent
}
/// Resolve explicit direct/category Agent policy against the current providers.
///
/// A direct model wins without a servability check. A category uses the
/// registry's ordered servability selection, including its existing
/// first-candidate fallback when no provider is currently available.
///
/// # Arguments
///
/// * `policy` - Agent model policy to resolve.
/// * `categories` - Configured category registry.
/// * `is_servable` - Predicate for current exact-model availability.
///
/// # Returns
///
/// The configured model, or `None` when the policy has no resolvable direct or
/// category route.
#[must_use]
pub fn resolve_configured_agent_model(
    policy: &ModelPolicy,
    categories: &CategoryRegistry,
    is_servable: &dyn Fn(&ModelRef) -> bool,
) -> Option<ModelRef> {
    policy.model.as_deref().map(ModelRef::new).or_else(|| {
        policy
            .category
            .as_deref()
            .and_then(|category| categories.resolve_servable(category, is_servable))
            .map(|resolved| resolved.model)
    })
}

/// Return the eligible remembered model for one Agent without allocating.
///
/// Direct model or category configuration suppresses remembered state. A
/// reasoning-only policy does not. The exact remembered model must still be
/// accepted by `is_servable`.
///
/// # Arguments
///
/// * `definition` - Catalog definition that owns explicit routing policy.
/// * `preference` - Optional model captured for the Agent's stable id.
/// * `is_servable` - Predicate for current exact-model availability.
///
/// # Returns
///
/// The borrowed remembered model only when it is eligible and servable.
#[must_use]
pub fn eligible_agent_model_preference<'a>(
    definition: &AgentDefinition<'_>,
    preference: Option<&'a ModelRef>,
    is_servable: &dyn Fn(&ModelRef) -> bool,
) -> Option<&'a ModelRef> {
    if definition.model_policy.model.is_some() || definition.model_policy.category.is_some() {
        return None;
    }
    preference.filter(|model| is_servable(model))
}

/// Apply a remembered model as the lowest-precedence default for one Agent.
///
/// A remembered model is eligible only when the Agent definition has neither a
/// direct model nor a category policy, and when `is_servable` accepts the exact
/// model reference. A reasoning-only policy does not suppress the preference.
///
/// # Arguments
///
/// * `agent` - Base specification whose model may receive the remembered value.
/// * `definition` - Catalog definition that supplies explicit model policy.
/// * `preference` - Optional remembered model captured for this Agent.
/// * `is_servable` - Predicate for whether the exact model is currently usable.
///
/// # Returns
///
/// The supplied `agent`, with its model replaced only when the remembered value
/// is eligible and servable.
#[must_use]
pub fn apply_agent_model_preference(
    mut agent: AgentSpec,
    definition: &AgentDefinition<'_>,
    preference: Option<&ModelRef>,
    is_servable: &dyn Fn(&ModelRef) -> bool,
) -> AgentSpec {
    if let Some(model) = eligible_agent_model_preference(definition, preference, is_servable) {
        agent.model = model.clone();
    }
    agent
}

/// One logical model category: an ordered list of concrete `provider/model`
/// candidates (`model` = first preference, `fallback` = the rest, tried in
/// order on unavailability), plus optional prompt/token shaping.
#[derive(Clone, Debug)]
pub struct CategoryEntry {
    /// Preferred concrete model.
    pub model: ModelRef,
    /// Ordered failover models after `model`.
    pub fallback: Vec<ModelRef>,
    /// Text appended into the agent system prompt for this category.
    pub prompt_append: String,
    /// Optional soft token budget hint for callers.
    pub token_budget: Option<u64>,
}

impl CategoryEntry {
    /// Build an entry with a single preferred model and prompt append.
    #[must_use]
    pub fn new(model: &str, prompt_append: &str) -> Self {
        Self {
            model: ModelRef::new(model),
            fallback: Vec::new(),
            prompt_append: prompt_append.to_string(),
            token_budget: None,
        }
    }

    /// Build an entry from an ordered candidate list (first = preferred model,
    /// rest = failover chain). Returns `None` when the list is empty, since a
    /// category with no concrete refs cannot resolve to anything servable.
    #[must_use]
    pub fn from_candidates(candidates: &[String]) -> Option<Self> {
        let mut refs = candidates
            .iter()
            .map(|candidate| candidate.trim())
            .filter(|candidate| !candidate.is_empty())
            .map(ModelRef::new);
        let model = refs.next()?;
        Some(Self {
            model,
            fallback: refs.collect(),
            prompt_append: String::new(),
            token_budget: None,
        })
    }
}

/// Concrete resolution of a named category for spawn/model selection.
#[derive(Clone, Debug)]
pub struct ResolvedCategory {
    /// Category key from config.
    pub category: String,
    /// Preferred model after resolution.
    pub model: ModelRef,
    /// Full ordered failover chain including preferred.
    pub fallback_chain: Vec<ModelRef>,
    /// Prompt append material.
    pub prompt_append: String,
    /// Optional token budget.
    pub token_budget: Option<u64>,
}

/// A directory of logical categories → ordered candidate lists. Empty by
/// default; entries come from config (`categories:` in `config.yaml`). There are
/// no built-in placeholder tiers — an unknown category simply fails to resolve
/// and the caller falls back down the precedence chain to the global default.
#[derive(Clone, Debug, Default)]
pub struct CategoryRegistry {
    entries: HashMap<String, CategoryEntry>,
}

impl CategoryRegistry {
    /// Empty registry (no categories resolve).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry directly from a set of concrete, config-driven entries.
    #[must_use]
    pub fn from_entries(entries: HashMap<String, CategoryEntry>) -> Self {
        Self { entries }
    }

    /// Overlay additional/replacement entries.
    #[must_use]
    pub fn with_overrides(mut self, overrides: HashMap<String, CategoryEntry>) -> Self {
        for (k, v) in overrides {
            self.entries.insert(k, v);
        }
        self
    }

    /// Whether no categories are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return each category's exact ordered model candidates in canonical key
    /// order. Prompt and token shaping are intentionally excluded because the
    /// spawn resolver currently discards those fields.
    #[must_use]
    pub fn resolution_candidates(&self) -> Vec<(String, Vec<ModelRef>)> {
        let mut entries = self
            .entries
            .iter()
            .map(|(category, entry)| {
                let mut candidates = Vec::with_capacity(1 + entry.fallback.len());
                candidates.push(entry.model.clone());
                candidates.extend(entry.fallback.iter().cloned());
                (category.clone(), candidates)
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// Resolve a category to its full ordered chain without a servability check
    /// (`model` = the first candidate). Prefer [`Self::resolve_servable`] on the
    /// live spawn path so failover picks the first *configured* provider.
    #[must_use]
    pub fn resolve(&self, category: &str) -> Option<ResolvedCategory> {
        let entry = self.entries.get(category)?;
        let mut fallback_chain = vec![entry.model.clone()];
        fallback_chain.extend(entry.fallback.clone());
        Some(ResolvedCategory {
            category: category.to_string(),
            model: entry.model.clone(),
            fallback_chain,
            prompt_append: entry.prompt_append.clone(),
            token_budget: entry.token_budget,
        })
    }

    /// Resolve a category, selecting the first candidate whose provider is
    /// servable per `is_servable` (ordered preference + failover, decision 8).
    /// When no candidate is servable the first candidate is returned as a
    /// best-effort model so the stream fails with a real provider error rather
    /// than a silent misroute. `fallback_chain` always carries the full order.
    #[must_use]
    pub fn resolve_servable(
        &self,
        category: &str,
        is_servable: impl Fn(&ModelRef) -> bool,
    ) -> Option<ResolvedCategory> {
        let entry = self.entries.get(category)?;
        let mut fallback_chain = vec![entry.model.clone()];
        fallback_chain.extend(entry.fallback.clone());
        let model = fallback_chain
            .iter()
            .find(|candidate| is_servable(candidate))
            .cloned()
            .unwrap_or_else(|| entry.model.clone());
        Some(ResolvedCategory {
            category: category.to_string(),
            model,
            fallback_chain,
            prompt_append: entry.prompt_append.clone(),
            token_budget: entry.token_budget,
        })
    }
}

/// Append a skills section to a base system prompt when `skills` is non-empty.
#[must_use]
pub fn inject_skills(base_prompt: &str, skills: &[String]) -> String {
    if skills.is_empty() {
        return base_prompt.to_string();
    }
    let mut out = base_prompt.to_string();
    out.push_str("\n\n## Skills\n");
    for skill in skills {
        out.push_str(skill);
        out.push('\n');
    }
    out
}

/// Build a member [`AgentSpec`] from a base agent, resolved category, and skill blobs.
#[must_use]
pub fn build_member_agent(
    base: &AgentSpec,
    resolved: &ResolvedCategory,
    skills: &[String],
) -> AgentSpec {
    let prompt = format!("{}\n\n{}", base.system_prompt, resolved.prompt_append);
    AgentSpec {
        name: base.name.clone(),
        model: resolved.model.clone(),
        system_prompt: inject_skills(&prompt, skills),
        workdir: base.workdir.clone(),
        reasoning: base.reasoning,
    }
}
