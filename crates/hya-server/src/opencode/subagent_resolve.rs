//! Resolve a spawned subagent's identity from the agent catalog.
//!
//! Historically every spawned member ran with the lead's hard-coded `build` agent,
//! so `subagent_type` (e.g. `explore`, `plan`) only affected the permission check —
//! members shared one prompt/model. This resolves the requested `subagent_type`
//! against the same catalog the primary session uses, applying its prompt, model,
//! and reasoning so `explore` and `plan` subagents actually differ.
//!
//! Per-agent permission enforcement for subagents is a follow-up (the catalog's
//! `permissions` are not yet layered onto the child session here).

use std::path::Path;

use hya_core::AgentSpec;
use hya_proto::{AgentName, ModelRef};

/// Resolve `subagent_type` into a fully-specialized [`AgentSpec`], starting from
/// `base` (the lead's spec, providing fallbacks for model/prompt/workdir). Unknown
/// types fall back to the native `general` subagent, then to `base` unchanged.
#[must_use]
pub fn resolve_subagent_agent(
    base: &AgentSpec,
    subagent_type: &str,
    workdir: &Path,
    include_global_agents: bool,
) -> AgentSpec {
    let mut agent = base.clone();
    agent.workdir = workdir.to_path_buf();
    agent.name = AgentName::new(subagent_type);

    let config = super::reasoning_options::load_opencode_config(workdir);
    let entries = super::agent_catalog::merged_entries(workdir, include_global_agents);
    let entry = entries
        .iter()
        .find(|e| e.name == subagent_type)
        .or_else(|| entries.iter().find(|e| e.name == "general"));

    if let Some(entry) = entry {
        if let Some(model) = &entry.model {
            agent.model = ModelRef::new(model);
        }
        let active_model = agent.model.clone();
        super::reference::apply_agent_entry(&mut agent, entry, &active_model, &config);
    }
    agent
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_proto::AgentName;
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

    #[test]
    fn explore_gets_its_own_prompt_not_build() {
        let workdir = std::env::temp_dir();
        let resolved = resolve_subagent_agent(&base(), "explore", &workdir, false);
        assert_eq!(resolved.name, AgentName::new("explore"));
        // The native explore agent carries its own prompt, so it must differ from
        // the base build prompt.
        assert_ne!(resolved.system_prompt, "BASE PROMPT");
        assert!(!resolved.system_prompt.is_empty());
    }

    #[test]
    fn unknown_type_keeps_name_and_falls_back() {
        let workdir = std::env::temp_dir();
        let resolved = resolve_subagent_agent(&base(), "nonexistent-xyz", &workdir, false);
        // Name reflects the requested type; model falls back to base when the
        // fallback agent does not override it.
        assert_eq!(resolved.name, AgentName::new("nonexistent-xyz"));
        assert_eq!(resolved.model, ModelRef::new("base-model"));
    }
}
