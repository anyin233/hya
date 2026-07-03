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
use hya_tool::InlineAgent;

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
    /// An ephemeral inline agent definition (decision 11). When present it
    /// supplies the system prompt + name and folds into the same precedence
    /// chain: inline `category:` sits at the frontmatter-category tier and inline
    /// `model:` at the frontmatter-model tier, each winning over the disk
    /// frontmatter within its tier; spawn-time overrides still win over both.
    pub inline_agent: Option<&'a InlineAgent>,
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

    // Level 4 (category tier): frontmatter `category:`, then inline `category:`
    // (inline is the more specific runtime spec, so it wins within the tier).
    if let Some(model) = entry
        .and_then(|entry| entry.category.as_deref())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = req
        .inline_agent
        .and_then(|inline| inline.category.as_deref())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    // Level 3 (model tier): frontmatter `model:`, then inline `model:` (inline
    // wins within the tier). The model tier is applied after the category tier so
    // any concrete model beats a category.
    if let Some(model) = entry.and_then(|entry| entry.model.as_deref()) {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = req
        .inline_agent
        .and_then(|inline| inline.model.as_deref())
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        agent.model = ModelRef::new(model);
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
    // An inline agent's own prompt + name override the catalog entry (its
    // ephemeral definition is authoritative for this run). Reasoning inherited
    // from a matched/fallback entry is model-derived and left intact.
    if let Some(inline) = req.inline_agent {
        if !inline.prompt.trim().is_empty() {
            agent.system_prompt = inline.prompt.clone();
        }
        if !inline.name.trim().is_empty() {
            agent.name = AgentName::new(&inline.name);
        }
    }
    agent
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_core::{CategoryEntry, CategoryRegistry};
    use hya_proto::AgentName;
    use hya_tool::InlineAgent;
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
            inline_agent: None,
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
            inline_agent: None,
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
            inline_agent: None,
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
            inline_agent: None,
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
            inline_agent: None,
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
            inline_agent: None,
        });
        assert_eq!(resolved.model, ModelRef::new("explicit/model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_agents_dir_discovers_and_inline_agent_supplies_prompt() {
        // Two behaviors in one atomic test: (1) an agent file under the new
        // `.claude/agents` discovery dir is found by `merged_entries` and resolves
        // its frontmatter category; (2) an inline ephemeral agent supplies its own
        // system prompt + name and folds into the SAME category precedence chain.
        let dir =
            std::env::temp_dir().join(format!("hya-subagent-claude-inline-{}", std::process::id()));
        let agent_dir = dir.join(".claude/agents");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("fuzzer.md"),
            "---\ncategory: deep\ndescription: Fuzz inputs\n---\nFuzzer prompt.\n",
        )
        .unwrap();

        let registry = registry_with(&[("deep", &["deep/model"]), ("quick", &["quick/model"])]);

        // (1) Discovery: `.claude/agents/fuzzer.md` is found and its `category: deep`
        // resolves via the registry; its prompt comes from the file.
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "fuzzer",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
            inline_agent: None,
        });
        assert_eq!(resolved.model, ModelRef::new("deep/model"));
        assert_eq!(resolved.system_prompt, "Fuzzer prompt.");

        // (2) Inline: an ephemeral agent (no disk file) supplies its own prompt +
        // name; its `category` participates in the precedence chain (~ frontmatter
        // category), resolving to `quick/model`.
        let inline = InlineAgent {
            name: "adhoc".to_string(),
            prompt: "INLINE PROMPT".to_string(),
            description: None,
            category: Some("quick".to_string()),
            model: None,
        };
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "adhoc",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: None,
            spawn_category: None,
            is_servable: &all_servable,
            inline_agent: Some(&inline),
        });
        assert_eq!(resolved.system_prompt, "INLINE PROMPT");
        assert_eq!(resolved.name, AgentName::new("adhoc"));
        assert_eq!(resolved.model, ModelRef::new("quick/model"));

        // Spawn-time explicit model still wins over the inline category.
        let resolved = resolve_subagent_agent(SubagentResolve {
            base: &base(),
            subagent_type: "adhoc",
            workdir: &dir,
            include_global_agents: false,
            categories: &registry,
            spawn_model: Some("explicit/model"),
            spawn_category: None,
            is_servable: &all_servable,
            inline_agent: Some(&inline),
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
            inline_agent: None,
        });
        // Frontmatter `model:` (precedence 3) wins over frontmatter `category:` (4).
        assert_eq!(resolved.model, ModelRef::new("explicit/frontmatter"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
