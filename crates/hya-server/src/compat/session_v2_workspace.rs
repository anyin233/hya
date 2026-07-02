use super::projection::CompatSessionInfo;
use crate::ServerState;

pub(super) fn retain(
    st: &ServerState,
    sessions: &mut Vec<CompatSessionInfo>,
    directory: Option<&str>,
    workspace: Option<&str>,
) {
    if let Some(directory) = directory {
        sessions.retain(|session| session.directory() == directory);
        return;
    }
    let Some(directory) = workspace
        .and_then(|id| super::worktree_git_lookup::directory_for_id(&st.agent.workdir, id))
    else {
        return;
    };
    let directory = directory.to_string_lossy();
    sessions.retain(|session| session.directory() == directory);
}
