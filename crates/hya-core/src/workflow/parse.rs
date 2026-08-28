//! Workflow file format and discovery.
//!
//! One workflow per file, either plain YAML or markdown whose YAML frontmatter
//! carries the whole definition (`workflow.hya.md`, mirroring the SKILL.md
//! convention). Discovery roots mirror skill discovery
//! ([`crate::prompt`] skills / `hya-tool::skill_catalog`): the project root
//! first, then the user config dir — first name wins.

use std::path::{Path, PathBuf};

use super::{WorkflowDef, WorkflowError};

/// Accepted workflow file extensions, in no particular order.
const WORKFLOW_EXTENSIONS: [&str; 3] = ["yaml", "yml", "md"];

/// Discovery roots for a project workdir: `<workdir>/.hya/workflows` first,
/// then `$HOME/.config/hya/workflows`.
#[must_use]
pub fn workflow_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".hya/workflows")];
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".config/hya/workflows"));
    }
    dirs
}

/// List candidate workflow files across discovery roots, ordered so earlier
/// entries shadow later ones (project before user; sorted within a directory).
#[must_use]
pub fn discover_workflow_files(workdir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in workflow_dirs_for_workdir(workdir) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter(|path| {
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| WORKFLOW_EXTENSIONS.contains(&ext))
            })
            .collect();
        names.sort();
        files.extend(names);
    }
    files
}

/// Load one workflow definition from disk.
///
/// # Errors
/// [`WorkflowError::Parse`] for unreadable files, malformed YAML/frontmatter,
/// or schema violations (unknown fields, missing required keys).
pub fn load_workflow_file(path: &Path) -> Result<WorkflowDef, WorkflowError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        WorkflowError::Parse(format!("cannot read {}: {error}", path.display()))
    })?;
    let source = extract_frontmatter(content);
    if source.trim().is_empty() {
        return Err(WorkflowError::Parse(format!(
            "{} has no YAML frontmatter definition",
            path.display()
        )));
    }
    serde_norway::from_str::<WorkflowDef>(&source)
        .map_err(|error| WorkflowError::Parse(format!("{}: {error}", path.display())))
}

/// Load a workflow by its declared `name`, scanning discovery roots in order.
///
/// # Errors
/// [`WorkflowError::Parse`] when a candidate file fails to parse;
/// [`WorkflowError::Invalid`] when no discovered workflow declares `name`.
pub fn load_workflow_by_name(workdir: &Path, name: &str) -> Result<WorkflowDef, WorkflowError> {
    for path in discover_workflow_files(workdir) {
        let def = load_workflow_file(&path)?;
        if def.name == name {
            return Ok(def);
        }
    }
    Err(WorkflowError::Invalid {
        workflow: name.to_string(),
        detail: format!(
            "no workflow named `{name}` under {}",
            workflow_dirs_for_workdir(workdir)
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Extract YAML frontmatter between the first two `---` fence lines, if any.
fn extract_frontmatter(content: String) -> String {
    let mut lines = content.lines();
    if lines.next().is_none_or(|first| first.trim_end() != "---") {
        return content;
    }
    let mut body = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            return body;
        }
        body.push_str(line);
        body.push('\n');
    }
    // An unclosed fence is treated as plain content and fails schema parsing
    // with a clear message instead of silently loading a partial definition.
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::FailurePolicy;

    const DEF: &str = r#"
name: demo
description: d
stages:
  - id: a
    agent: worker
    prompt: "hi {{inputs.x}}"
"#;

    #[test]
    fn frontmatter_and_yaml_both_parse() {
        let md = format!("---\n{}---\nFree-form docs live after the fence.\n", DEF);
        let tmp = std::env::temp_dir().join("hya-workflow-parse-test");
        std::fs::create_dir_all(&tmp).unwrap_or_default();
        let md_path = tmp.join("demo.workflow.hya.md");
        std::fs::write(&md_path, md).unwrap_or_default();
        let yaml_path = tmp.join("demo.yaml");
        std::fs::write(&yaml_path, DEF).unwrap_or_default();

        let from_md = load_workflow_file(&md_path).unwrap_or_else(|e| panic!("md: {e}"));
        assert_eq!(from_md.name, "demo");
        let from_yaml = load_workflow_file(&yaml_path).unwrap_or_else(|e| panic!("yaml: {e}"));
        assert_eq!(from_yaml, from_md);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let bad = format!("{DEF}oops_field: true\n");
        let error = serde_norway::from_str::<WorkflowDef>(&bad)
            .err()
            .unwrap_or_else(|| panic!("unknown field must fail"));
        assert!(
            error.to_string().contains("oops_field") || error.to_string().contains("unknown"),
            "{error}"
        );
    }

    #[test]
    fn defaults_apply() {
        let def = serde_norway::from_str::<WorkflowDef>(DEF).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(def.on_member_failure, FailurePolicy::FailFast);
        assert_eq!(def.stages[0].needs.len(), 0);
    }

    #[test]
    fn discovery_orders_project_before_user() {
        // Only asserts root ordering logic; user home may not exist in CI sandboxes.
        let dirs = workflow_dirs_for_workdir(Path::new("/tmp/project"));
        assert_eq!(dirs[0], PathBuf::from("/tmp/project/.hya/workflows"));
    }
}
