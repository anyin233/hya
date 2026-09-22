//! Skill tool implementation.
use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Action, Resource, SkillCatalogOrigin, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FILE_SAMPLE_LIMIT: usize = 10;

pub struct SkillTool;

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
        let (base, files) = match info.origin {
            SkillCatalogOrigin::Embedded => (
                "This skill is embedded in hya; it has no filesystem base directory or sampled files."
                    .to_string(),
                String::new(),
            ),
            SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => {
                let dir = info.dir.as_deref().unwrap_or(Path::new(""));
                let files = sample_files(dir, FILE_SAMPLE_LIMIT);
                (
                    format!(
                        "Base directory for this skill: file://{}\nRelative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\nNote: file list is sampled.",
                        dir.to_string_lossy()
                    ),
                    format!(
                        "\n\n<skill_files>\n{}\n</skill_files>",
                        files
                            .iter()
                            .map(|file| format!("<file>{}</file>", file.to_string_lossy()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                )
            }
        };
        let origin = match info.origin {
            SkillCatalogOrigin::Filesystem => "filesystem",
            SkillCatalogOrigin::Embedded => "embedded",
            SkillCatalogOrigin::Virtual => "virtual",
        };
        let output = format!(
            "<skill_content name=\"{}\">\n# Skill: {}\n\n{}\n\n{}{}\n</skill_content>",
            info.name,
            info.name,
            info.content.trim(),
            base,
            files
        );
        Ok(json!({
            "title": format!("Loaded skill: {}", info.name),
            "output": output,
            "metadata": {
                "name": info.name,
                "origin": origin,
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
