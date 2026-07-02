use std::path::PathBuf;

use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Info {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    directory: String,
}

impl Info {
    pub(super) fn new(name: String, branch: Option<String>, directory: String) -> Self {
        Self {
            name,
            branch,
            directory,
        }
    }

    pub(super) fn from_path(directory: String, branch: Option<String>) -> Self {
        let path = PathBuf::from(&directory);
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .map_or_else(|| "worktree".to_string(), ToString::to_string);
        Self::new(name, branch, directory)
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    pub(super) fn directory(&self) -> &str {
        &self.directory
    }

    pub(super) fn into_directory(self) -> String {
        self.directory
    }
}
