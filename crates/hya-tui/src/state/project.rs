#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectState {
    pub current_path: Option<String>,
    pub workspace: Option<WorkspaceState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    pub id: String,
    pub directory: String,
}
