use std::collections::HashMap;

use hya_proto::ModelRef;

use crate::engine::AgentSpec;

/// One logical model category: an ordered list of concrete `provider/model`
/// candidates (`model` = first preference, `fallback` = the rest, tried in
/// order on unavailability), plus optional prompt/token shaping.
#[derive(Clone, Debug)]
pub struct CategoryEntry {
    pub model: ModelRef,
    pub fallback: Vec<ModelRef>,
    pub prompt_append: String,
    pub token_budget: Option<u64>,
}

impl CategoryEntry {
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

#[derive(Clone, Debug)]
pub struct ResolvedCategory {
    pub category: String,
    pub model: ModelRef,
    pub fallback_chain: Vec<ModelRef>,
    pub prompt_append: String,
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
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry directly from a set of concrete, config-driven entries.
    #[must_use]
    pub fn from_entries(entries: HashMap<String, CategoryEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn with_overrides(mut self, overrides: HashMap<String, CategoryEntry>) -> Self {
        for (k, v) in overrides {
            self.entries.insert(k, v);
        }
        self
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
