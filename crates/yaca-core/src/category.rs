use std::collections::HashMap;

use yaca_proto::ModelRef;

use crate::engine::AgentSpec;

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
}

#[derive(Clone, Debug)]
pub struct ResolvedCategory {
    pub category: String,
    pub model: ModelRef,
    pub fallback_chain: Vec<ModelRef>,
    pub prompt_append: String,
    pub token_budget: Option<u64>,
}

pub struct CategoryRegistry {
    entries: HashMap<String, CategoryEntry>,
}

impl CategoryRegistry {
    #[must_use]
    pub fn builtins() -> Self {
        let mut entries = HashMap::new();
        entries.insert(
            "quick".to_string(),
            CategoryEntry::new("tier-cheap", "Be fast and minimal."),
        );
        entries.insert(
            "deep".to_string(),
            CategoryEntry::new("tier-strong", "Think deeply and thoroughly."),
        );
        entries.insert(
            "ultrabrain".to_string(),
            CategoryEntry::new("tier-max", "Hardest reasoning; leave no stone unturned."),
        );
        entries.insert(
            "writing".to_string(),
            CategoryEntry::new("tier-writer", "Write clear, well-structured prose."),
        );
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
    }
}
