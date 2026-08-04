use std::path::Path;

use hya_tool::discover_skills;

use serde::Serialize;

const CUSTOMIZE_COMPAT_BODY: &str = include_str!("skill_templates/customize-compat.md");
const CUSTOMIZE_COMPAT_DESCRIPTION: &str = "Use ONLY when the user is editing or creating compat's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing compat skills, plugins, MCP servers, or permission rules. Do not use for native agent authoring (see agent-bundle-authoring), the user's own application code, or any project that is not configuring compat itself.";
const AGENT_BUNDLE_AUTHORING_BODY: &str = include_str!("skill_templates/agent-bundle-authoring.md");
const AGENT_BUNDLE_AUTHORING_DESCRIPTION: &str = "Use when authoring and packaging 0.34.11 public AgentBundles: static process-free bundles or activation-scoped Bun Compat sidecars, exact bundle.hya.md closure, install/info commands, stable AgentName bytes, role/can_spawn/lifecycle, harness resource views, and private/unsupported boundaries. Do not use for compat opencode.json customization, external model loops, raw Rust activation, or Bundle-declared MCP.";
const SECURE_SELF_UPDATE_BODY: &str = include_str!("skill_templates/secure-self-update.md");
const SECURE_SELF_UPDATE_DESCRIPTION: &str = "Use when verifying, staging, recovering, or owner-activating an independent hya release with hya-updater: signed metadata, local package fetch, immutable staging, smoke subprocess, activation journal/selector, anti-rollback floor, and install.sh break-glass. Do not use for bundle install, plugin load, or to skip the owner activation gate.";

#[derive(Clone, Serialize)]
pub(in crate::compat) struct SkillInfo {
    pub(in crate::compat) name: String,
    pub(in crate::compat) description: String,
    pub(in crate::compat) location: String,
    pub(in crate::compat) content: String,
}

pub(in crate::compat) fn list(workdir: &Path) -> Vec<SkillInfo> {
    let mut skills = discover_skills(workdir)
        .into_iter()
        .map(|skill| SkillInfo {
            name: skill.name,
            description: skill.description,
            location: skill.path.to_string_lossy().into_owned(),
            content: skill.content,
        })
        .collect::<Vec<_>>();
    if !skills.iter().any(|skill| skill.name == "customize-compat") {
        skills.push(SkillInfo {
            name: "customize-compat".to_string(),
            description: CUSTOMIZE_COMPAT_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: CUSTOMIZE_COMPAT_BODY.to_string(),
        });
    }
    if !skills
        .iter()
        .any(|skill| skill.name == "agent-bundle-authoring")
    {
        skills.push(SkillInfo {
            name: "agent-bundle-authoring".to_string(),
            description: AGENT_BUNDLE_AUTHORING_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: AGENT_BUNDLE_AUTHORING_BODY.to_string(),
        });
    }
    if !skills
        .iter()
        .any(|skill| skill.name == "secure-self-update")
    {
        skills.push(SkillInfo {
            name: "secure-self-update".to_string(),
            description: SECURE_SELF_UPDATE_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: SECURE_SELF_UPDATE_BODY.to_string(),
        });
    }
    skills
}
