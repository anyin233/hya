//! Workspace roots of a session (ADR-0024), resolved at every turn start
//! into [`hya_tool::ToolCtx::roots`].
//!
//! A Project session sees its Project's roots in order; a session whose
//! workdir lies inside none of them (a Project edited after the session was
//! created) gets the workdir prepended so the working directory always stays
//! in scope. A temporary session, a session with no Project, and a session
//! whose Project was deleted see only `[workdir]`. Roots are read from the
//! store fresh per turn, so a Project edit applies to the next turn.

use std::path::{Path, PathBuf};

use hya_proto::{Projection, SessionId, SessionKind};

use super::SessionEngine;

impl SessionEngine {
    /// Resolve the workspace roots for one turn of `session` (folded into
    /// `projection`), whose tools run in `workdir`.
    ///
    /// Never fails: a deleted Project or a store read failure falls back to
    /// `[workdir]` with a warning, the narrowest scope.
    pub(crate) async fn session_roots(
        &self,
        session: SessionId,
        projection: &Projection,
        workdir: &Path,
    ) -> Vec<PathBuf> {
        let project = match (projection.session.kind, projection.session.project) {
            (SessionKind::Project, Some(project)) => project,
            _ => return vec![workdir.to_path_buf()],
        };
        match self.store.get_project(project).await {
            Ok(Some(found)) => merge_roots(workdir, &found.roots),
            Ok(None) => {
                tracing::warn!(
                    %session,
                    %project,
                    "session project was deleted; scoping the turn to its workdir"
                );
                vec![workdir.to_path_buf()]
            }
            Err(error) => {
                tracing::warn!(
                    %session,
                    %project,
                    %error,
                    "reading the session project failed; scoping the turn to its workdir"
                );
                vec![workdir.to_path_buf()]
            }
        }
    }
}

/// A Project's `roots` in order, with `workdir` prepended when it lies
/// inside none of them; duplicates dropped (first occurrence wins).
fn merge_roots(workdir: &Path, roots: &[String]) -> Vec<PathBuf> {
    let inside = roots
        .iter()
        .any(|root| workdir.starts_with(Path::new(root)));
    let mut merged: Vec<PathBuf> = Vec::with_capacity(roots.len() + 1);
    if !inside {
        merged.push(workdir.to_path_buf());
    }
    for root in roots.iter().map(PathBuf::from) {
        if !merged.contains(&root) {
            merged.push(root);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn paths(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn workdir_equal_to_the_primary_root_keeps_the_project_roots() {
        assert_eq!(
            merge_roots(Path::new("/w/a"), &roots(&["/w/a", "/w/b"])),
            paths(&["/w/a", "/w/b"])
        );
    }

    #[test]
    fn workdir_inside_a_later_root_keeps_the_project_roots() {
        assert_eq!(
            merge_roots(Path::new("/w/b/src"), &roots(&["/w/a", "/w/b"])),
            paths(&["/w/a", "/w/b"])
        );
    }

    #[test]
    fn workdir_outside_every_root_is_prepended() {
        assert_eq!(
            merge_roots(Path::new("/elsewhere"), &roots(&["/w/a", "/w/b"])),
            paths(&["/elsewhere", "/w/a", "/w/b"])
        );
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside() {
        assert_eq!(
            merge_roots(Path::new("/w/ab"), &roots(&["/w/a"])),
            paths(&["/w/ab", "/w/a"])
        );
    }
}
