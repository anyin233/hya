//! Resolve a spawned subagent's identity from the agent catalog.
//!
//! Historically every spawned member ran with the lead's hard-coded `build` agent,
//! so `subagent_type` (e.g. `explore`, `plan`) only affected the permission check —
//! members shared one prompt/model. This resolves the requested `subagent_type`
//! against the same catalog the primary session uses, applying its prompt, model,
//! and reasoning so `explore` and `plan` subagents actually differ.
//!
//! It also applies the decision-9 model-selection precedence at spawn time
//! (highest wins): spawn-time explicit `model` → spawn-time `category` override →
//! frontmatter `model:` → frontmatter `category:` → global default (`base.model`).
//! Categories resolve to a concrete `provider/model` via [`CategoryRegistry`] with
//! ordered failover to the first candidate whose provider is servable.
//!
//! Per-agent permission enforcement for subagents is a follow-up (the catalog's
//! `permissions` are not yet layered onto the child session here).

use std::path::Path;

use hya_core::{AgentSpec, CategoryRegistry};
use hya_proto::{AgentName, ModelRef};

/// Inputs to [`resolve_subagent_agent`]. Bundled into a struct so the spawn-time
/// overrides, category registry, and servability predicate travel together
/// without exploding the argument list.
pub struct SubagentResolve<'a> {
    /// The lead's spec, supplying fallbacks for model/prompt/workdir.
    pub base: &'a AgentSpec,
    /// Requested agent type (e.g. `explore`, `plan`, or a custom agent name).
    pub subagent_type: &'a str,
    pub workdir: &'a Path,
    pub include_global_agents: bool,
    /// Config-driven logical model categories.
    pub categories: &'a CategoryRegistry,
    /// Spawn-time explicit `model` override (precedence 1, highest). Empty → unset.
    pub spawn_model: Option<&'a str>,
    /// Spawn-time `category` override (precedence 2). Empty → unset.
    pub spawn_category: Option<&'a str>,
    /// "Is this concrete ref servable?" — used for ordered category failover.
    /// Typically `|m| router.resolve(m).is_some()`.
    pub is_servable: &'a dyn Fn(&ModelRef) -> bool,
}

/// Resolve `subagent_type` into a fully-specialized [`AgentSpec`]. Unknown types
/// fall back to the native `general` subagent, then to `base` unchanged. The
/// final `AgentSpec.model` reflects the winning precedence source (decision 9).
#[must_use]
pub fn resolve_subagent_agent(req: SubagentResolve<'_>) -> AgentSpec {
    let mut agent = req.base.clone();
    agent.workdir = req.workdir.to_path_buf();
    agent.name = AgentName::new(req.subagent_type);

    let config = super::reasoning_options::load_compat_config(req.workdir);
    let entries = super::agent_catalog::merged_entries(req.workdir, req.include_global_agents);
    let entry = entries
        .iter()
        .find(|e| e.name == req.subagent_type)
        .or_else(|| entries.iter().find(|e| e.name == "general"));

    // Precedence, applied low → high so each higher source overwrites the model.
    // Level 5 (global default = `base.model`) is already in `agent.model`.
    let resolve_category = |name: &str| -> Option<ModelRef> {
        req.categories
            .resolve_servable(name, req.is_servable)
            .map(|resolved| resolved.model)
    };

    if let Some(entry) = entry {
        // Level 4: frontmatter `category:`.
        if let Some(model) = entry.category.as_deref().and_then(&resolve_category) {
            agent.model = model;
        }
        // Level 3: frontmatter `model:`.
        if let Some(model) = entry.model.as_deref() {
            agent.model = ModelRef::new(model);
        }
    }

    // Level 2: spawn-time `category` override.
    if let Some(model) = req
        .spawn_category
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    // Level 1: spawn-time explicit `model` override (highest).
    if let Some(model) = req.spawn_model.map(str::trim).filter(|m| !m.is_empty()) {
        agent.model = ModelRef::new(model);
    }

    // Apply prompt + reasoning using the final resolved model.
    if let Some(entry) = entry {
        let active_model = agent.model.clone();
        super::reference::apply_agent_entry(&mut agent, entry, &active_model, &config);
    }
    agent
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_core::{CategoryEntry, CategoryRegistry};
    use hya_proto::AgentName;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn base() -> AgentSpec {
        AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "BASE PROMPT".to_string(),
            workdir: PathBuf::from("/tmp"),
            reasoning: None,
        }
    }

    fn empty_registry() -> CategoryRegistry {
        CategoryRegistry::new()
    }

    fn all_servable(_: &ModelRef) -> bool {
        true
    }

    #[test]
    fn explore_gets_its_own_prompt_not_build() {
        let workdir = std::env::temp_dir();
        let registry = empty_registry();
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "explore",
            workdir: &workdir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
        });
        assert_eq!(resolved.name, AgentName::new("explore"));
        // The native explore agent carries its own prompt, so it must differ from
        // the base build prompt.
        assert_ne!(resolved.system_prompt, "BASE PROMPT");
        assert!(!resolved.system_prompt.is_empty());
    }

    #[test]
    fn unknown_type_keeps_name_and_falls_back() {
        let workdir = std::env::temp_dir();
        let registry = empty_registry();
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "nonexistent-xyz",
            workdir: &workdir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
        });
        // Name reflects the requested type; model falls back to base when the
        // fallback agent does not override it.
        assert_eq!(resolved.name, AgentName::new("nonexistent-xyz"));
        assert_eq!(resolved.model, ModelRef::new("base-model"));
    }

    fn registry_with(entries: &[(&str, &[&str])]) -> CategoryRegistry {
        let mut overrides = HashMap::new();
        for (name, candidates) in entries {
            let candidates: Vec<String> = candidates.iter().map(|c| (*c).to_string()).collect();
            overrides.insert(
                (*name).to_string(),
                CategoryEntry::from_candidates(&candidates).unwrap(),
            );
        }
        CategoryRegistry::new().with_overrides(overrides)
    }

    #[test]
    fn frontmatter_category_resolves_via_registry() {
        // A disk agent with `category: deep` frontmatter, no explicit model.
        let dir = std::env::temp_dir().join(format!("hya-subagent-cat-{}", std::process::id()));
        let agent_dir = dir.join(".opencode/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("researcher.md"),
            "---\ncategory: deep\n---\nResearch prompt.\n",
        )
        .unwrap();

        let registry = registry_with(&[("deep", &["primary/opus", "backup/sonnet"])]);
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "researcher",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
        });
        assert_eq!(resolved.model, ModelRef::new("primary/opus"));

        // With the primary provider absent, failover picks candidate #2.
        let failover = |m: &ModelRef| m.as_str() != "primary/opus";
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "researcher",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &failover,
        });
        assert_eq!(resolved.model, ModelRef::new("backup/sonnet"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_category_override_beats_frontmatter_category() {
        let dir =
            std::env::temp_dir().join(format!("hya-subagent-cat-override-{}", std::process::id()));
        let agent_dir = dir.join(".opencode/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("researcher.md"),
            "---\ncategory: deep\n---\nResearch prompt.\n",
        )
        .unwrap();

        let registry = registry_with(&[("deep", &["deep/model"]), ("quick", &["quick/model"])]);
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "researcher",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: Some("quick"),
            is_servable: &all_servable,
        });
        // Spawn-time category (precedence 2) wins over frontmatter category (4).
        assert_eq!(resolved.model, ModelRef::new("quick/model"));

        // Spawn-time explicit model (precedence 1) wins over everything.
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "researcher",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: Some("explicit/model"),
            spawn_category: Some("quick"),
            is_servable: &all_servable,
        });
        assert_eq!(resolved.model, ModelRef::new("explicit/model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frontmatter_model_beats_frontmatter_category() {
        let dir =
            std::env::temp_dir().join(format!("hya-subagent-model-wins-{}", std::process::id()));
        let agent_dir = dir.join(".opencode/agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("researcher.md"),
            "---\ncategory: deep\nmodel: explicit/frontmatter\n---\nResearch prompt.\n",
        )
        .unwrap();

        let registry = registry_with(&[("deep", &["deep/model"])]);
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "researcher",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
        });
        // Frontmatter `model:` (precedence 3) wins over frontmatter `category:` (4).
        assert_eq!(resolved.model, ModelRef::new("explicit/frontmatter"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
