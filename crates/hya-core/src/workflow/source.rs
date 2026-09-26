//! Filesystem discovery for compiled Workflow documents.

use std::path::{Path, PathBuf};

use super::{CompiledWorkflow, WorkflowError};

/// The project-tier Workflow directory under one root (a Project root, a
/// plain directory, or a Session workdir).
#[must_use]
pub fn workflow_project_dir(root: &Path) -> PathBuf {
    root.join(".hya/workflows")
}

/// The user-tier Workflow directory, when the process has a home directory.
#[must_use]
pub fn workflow_user_dir() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".config/hya/workflows"))
}

/// Return Workflow discovery roots in project-before-user precedence order.
#[must_use]
pub fn workflow_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workflow_project_dir(workdir)];
    dirs.extend(workflow_user_dir());
    dirs
}

/// List candidate `*.hya.md` Workflow files in one explicit directory.
#[must_use]
pub fn discover_workflow_files_in_root(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_workflow_source(path))
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Compile one Workflow document from disk.
///
/// # Errors
/// Returns [`WorkflowError::Source`] for an unreadable file and
/// [`WorkflowError::Compile`] for an invalid document.
pub fn load_workflow_file(path: &Path) -> Result<CompiledWorkflow, WorkflowError> {
    let content = std::fs::read_to_string(path).map_err(|error| WorkflowError::Source {
        source_name: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let source_name = path.display().to_string();
    hya_workflow::compile(hya_workflow::WorkflowSource::new(&source_name, &content))
        .map_err(WorkflowError::Compile)
}

/// Return whether a path uses the only accepted Workflow source suffix.
fn is_workflow_source(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".hya.md"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const SOURCE: &str = r#"---
kind: Workflow
name: demo
description: One compiled Workflow.
nodes:
  run:
    agent: worker
    directive: Run.
---
flowchart TD
  run
"#;

    /// Create one process-unique temporary test root.
    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "hya-workflow-source-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    /// A `*.hya.md` source compiles through the public compiler path.
    #[test]
    fn markdown_workflow_compiles_from_disk() {
        let root = test_root("compile");
        std::fs::create_dir_all(&root).unwrap_or_else(|error| panic!("create fixture: {error}"));
        let path = root.join("demo.hya.md");
        std::fs::write(&path, SOURCE).unwrap_or_else(|error| panic!("write fixture: {error}"));

        let workflow =
            load_workflow_file(&path).unwrap_or_else(|error| panic!("compile fixture: {error}"));
        assert_eq!(workflow.definition().name(), "demo");
        assert_eq!(workflow.plan().stages()[0].id(), "run");

        let _ = std::fs::remove_dir_all(root);
    }

    /// Explicit-root discovery accepts only the compiled Markdown suffix and sorts names.
    #[test]
    fn discovery_rejects_legacy_yaml_and_orders_sources() {
        let root = test_root("discovery");
        let directory = root.join(".hya/workflows");
        std::fs::create_dir_all(&directory)
            .unwrap_or_else(|error| panic!("create fixture: {error}"));
        for name in ["z.hya.md", "a.hya.md", "legacy.yaml", "notes.md"] {
            std::fs::write(directory.join(name), SOURCE)
                .unwrap_or_else(|error| panic!("write fixture: {error}"));
        }

        let files = discover_workflow_files_in_root(&directory);
        let names = files
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>();
        assert_eq!(names, ["a.hya.md", "z.hya.md"]);

        let _ = std::fs::remove_dir_all(root);
    }

    /// Project discovery root always has precedence over the user root.
    #[test]
    fn project_root_precedes_user_root() {
        let dirs = workflow_dirs_for_workdir(Path::new("/tmp/project"));
        assert_eq!(dirs[0], PathBuf::from("/tmp/project/.hya/workflows"));
    }

    /// `workflow_project_dir` is the exact join `workflow_dirs_for_workdir`
    /// uses for its own project root, so a multi-root caller can derive one
    /// discovery directory per Project root without going through a workdir.
    #[test]
    fn project_dir_matches_the_project_tier_of_workflow_dirs_for_workdir() {
        let root = Path::new("/tmp/one-of-several-roots");
        assert_eq!(
            workflow_project_dir(root),
            workflow_dirs_for_workdir(root)[0]
        );
    }
}
