//! Where a catalog read looks: the catalog scope (`Global`, a plain
//! `Directory`, or a registered `Project` with its roots) plus the
//! directory the read is for.
//!
//! One value drives every catalog tier of a read so they cannot disagree:
//! the runtime binding (agents, bundle overlay) binds the scope, and the
//! inert disk tiers (skills, commands) look in the directory first, then in
//! each Project root in order — the first definition of a name wins.

use std::path::{Path, PathBuf};

use hya_core::{CatalogScope, CoreError, TurnBinding};
use hya_proto::SessionId;

use crate::ServerState;

#[derive(Clone, Debug)]
pub(crate) struct CatalogPlace {
    scope: CatalogScope,
    workdir: Option<PathBuf>,
}

impl CatalogPlace {
    /// The global (project-less) view: no directory, no Project.
    pub(crate) fn global() -> Self {
        Self {
            scope: CatalogScope::Global,
            workdir: None,
        }
    }

    /// The place of a catalog read for `directory`: its Project when it lies
    /// inside one, else the plain directory; no directory is global.
    pub(crate) async fn for_directory(st: &ServerState, directory: Option<PathBuf>) -> Self {
        let scope = st
            .engine
            .catalog_scope_for_directory(directory.as_deref())
            .await;
        match scope {
            CatalogScope::Global => Self::global(),
            scope => Self {
                scope,
                workdir: directory,
            },
        }
    }

    /// The place `session`'s turns see: its catalog scope and its workdir.
    pub(crate) async fn for_session(
        st: &ServerState,
        session: SessionId,
    ) -> Result<Self, CoreError> {
        let workdir = crate::support::reference::session_workdir(st, session).await?;
        let scope = st.engine.catalog_scope_for_session(session).await?;
        Ok(Self {
            scope,
            workdir: Some(workdir),
        })
    }

    /// A plain directory (no Project), for unit tests.
    #[cfg(test)]
    pub(crate) fn directory_for_tests(dir: &str) -> Self {
        Self {
            scope: CatalogScope::Directory(PathBuf::from(dir)),
            workdir: Some(PathBuf::from(dir)),
        }
    }

    pub(crate) fn scope(&self) -> &CatalogScope {
        &self.scope
    }

    /// The directory the read is for; `None` for the global view.
    pub(crate) fn workdir(&self) -> Option<&Path> {
        self.workdir.as_deref()
    }

    /// The Project's roots in order; empty outside a Project.
    pub(crate) fn roots(&self) -> &[PathBuf] {
        match &self.scope {
            CatalogScope::Project { roots, .. } => roots,
            CatalogScope::Global | CatalogScope::Directory(_) => &[],
        }
    }

    /// The directories of the inert disk tiers in precedence order: the
    /// workdir, then each root; each directory once.
    pub(crate) fn dirs(&self) -> Vec<&Path> {
        let mut dirs: Vec<&Path> = Vec::new();
        for dir in self
            .workdir
            .as_deref()
            .into_iter()
            .chain(self.roots().iter().map(PathBuf::as_path))
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// Bind the runtime of this place (agents, the scope's bundle overlay).
    pub(crate) async fn bind(&self, st: &ServerState) -> Result<TurnBinding, CoreError> {
        st.engine
            .bind_scope_runtime(&self.scope, self.workdir().unwrap_or(Path::new("")))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_put_the_workdir_first_then_each_root_once() {
        let place = CatalogPlace {
            scope: CatalogScope::Project {
                id: hya_proto::ProjectId::new(),
                roots: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            },
            workdir: Some(PathBuf::from("/b")),
        };
        assert_eq!(place.dirs(), vec![Path::new("/b"), Path::new("/a")]);
        assert!(CatalogPlace::global().dirs().is_empty());
    }
}
