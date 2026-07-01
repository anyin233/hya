use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::permission::{Action, Resource};
use crate::skill_catalog::{discover_skills, discover_skills_from_dirs};
use crate::tool::{Tool, ToolCtx, ToolError};

const FILE_SAMPLE_LIMIT: usize = 10;

#[derive(Clone)]
pub struct SkillPlane {
    roots: SkillRoots,
}

#[derive(Clone)]
enum SkillRoots {
    DefaultForWorkdir,
    ExplicitDirs(Arc<Vec<PathBuf>>),
}

impl Default for SkillPlane {
    fn default() -> Self {
        Self {
            roots: SkillRoots::DefaultForWorkdir,
        }
    }
}

impl SkillPlane {
    #[must_use]
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            roots: SkillRoots::ExplicitDirs(Arc::new(dirs)),
        }
    }

    fn require(&self, workdir: &Path, name: &str) -> Result<SkillInfo, SkillError> {
        let skills = match &self.roots {
            SkillRoots::DefaultForWorkdir => discover_skills(workdir),
            SkillRoots::ExplicitDirs(dirs) => discover_skills_from_dirs(dirs),
        };
        let Some(skill) = skills.into_iter().find(|skill| skill.name == name) else {
            return Err(SkillError::NotFound(name.to_string()));
        };
        Ok(SkillInfo {
            name: skill.name,
            dir: canonical_or_self(&skill.dir),
            content: skill.content,
        })
    }
}

#[derive(Debug, Error)]
enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
}

struct SkillInfo {
    name: String,
    dir: PathBuf,
    content: String,
}

pub(crate) struct SkillTool;

#[derive(Deserialize)]
struct SkillInput {
    name: String,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("skill"),
            description: "Load a specialized skill listed in the system prompt.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The name of the skill from available_skills"
                    }
                },
                "required": ["name"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SkillInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        ctx.permission
            .assert(Action::Skill, Resource::Skill(input.name.clone()))
            .await?;
        let info = ctx
            .skills
            .require(&ctx.workdir, &input.name)
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let files = sample_files(&info.dir, FILE_SAMPLE_LIMIT);
        let output = format!(
            "<skill_content name=\"{}\">\n# Skill: {}\n\n{}\n\nBase directory for this skill: file://{}\nRelative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\nNote: file list is sampled.\n\n<skill_files>\n{}\n</skill_files>\n</skill_content>",
            info.name,
            info.name,
            info.content.trim(),
            info.dir.to_string_lossy(),
            files
                .iter()
                .map(|file| format!("<file>{}</file>", file.to_string_lossy()))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(json!({
            "title": format!("Loaded skill: {}", info.name),
            "output": output,
            "metadata": {
                "name": info.name,
                "dir": info.dir,
            },
        }))
    }
}

fn sample_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(dir, limit, &mut files);
    files
}

fn collect_files(dir: &Path, limit: usize, files: &mut Vec<PathBuf>) {
    if files.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if files.len() >= limit {
            return;
        }
        if path.is_dir() {
            collect_files(&path, limit, files);
        } else if path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
            files.push(canonical_or_self(&path));
        }
    }
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
