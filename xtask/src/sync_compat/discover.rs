use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::sync_compat::cli::Args;

#[derive(Debug, Deserialize)]
pub(crate) struct CompatConfig {
    #[serde(default)]
    skills: CompatSkills,
    #[serde(default)]
    mcp: BTreeMap<String, CompatMcp>,
}

#[derive(Debug, Default, Deserialize)]
struct CompatSkills {
    #[serde(default)]
    paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct CompatMcp {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct McpCandidate {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct SkillCandidate {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
}

pub(crate) fn load_compat_config(path: &Path) -> Result<CompatConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read Compat config {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse Compat config {}", path.display()))
}

pub(crate) fn collect_skill_roots(args: &Args, config: &CompatConfig) -> Vec<PathBuf> {
    let mut roots = args.compat_skill_roots.clone();
    roots.extend(config.skills.paths.iter().cloned());
    roots
}

pub(crate) fn collect_skills(roots: &[PathBuf]) -> Result<Vec<SkillCandidate>> {
    let mut skills = Vec::new();
    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let skill_path = dir.join("SKILL.md");
            let raw = match std::fs::read_to_string(&skill_path) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            if let Some(name) = parse_skill_name(&raw) {
                skills.push(SkillCandidate { name, dir });
            }
        }
    }
    Ok(skills)
}

pub(crate) fn collect_supported_mcp(config: &CompatConfig) -> Vec<McpCandidate> {
    config
        .mcp
        .iter()
        .filter(|(_, server)| server.kind == "local" && server.enabled != Some(false))
        .map(|(name, server)| McpCandidate {
            name: name.clone(),
            command: server.command.clone(),
            env: server.environment.clone(),
        })
        .collect()
}

fn parse_skill_name(raw: &str) -> Option<String> {
    let after = raw.strip_prefix("---")?;
    let end = after.find("\n---")?;
    let frontmatter = &after[..end];
    frontmatter.lines().find_map(|line| {
        line.strip_prefix("name:")
            .map(|value| value.trim().trim_matches('"').to_string())
    })
}
