//! Read-only CLI inspection helper for discovering workflows without starting a runtime.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_engine::{AgentCatalog, CatalogScope};
use hya_workflow_bundle_core::{WorkflowCatalog, WorkflowCatalogRoots};

use crate::{ProjectScopeRefresh, bundle_admin, first_party_catalogs};

/// Inspect prepared Workflow sources for a working directory without starting a runtime.
///
/// An existing database is opened read-only to resolve Project roots. Bundle
/// diagnostics are delivered in discovery order through `warn`.
///
/// # Errors
/// Returns database, bundle discovery, or catalog construction errors.
pub async fn inspect_workflow_catalog_read_only(
    db: &str,
    workdir: &Path,
    mut warn: impl FnMut(&str),
) -> anyhow::Result<WorkflowCatalog> {
    let project = if !db.is_empty() && Path::new(db).try_exists()? {
        hya_store::SessionStore::connect_read_only(db)
            .await
            .with_context(|| format!("open existing database read-only at {db}"))?
            .resolve_project_by_path(&workdir.to_string_lossy())
            .await?
    } else {
        None
    };
    let scope = project.map_or_else(
        || CatalogScope::Directory(workdir.to_path_buf()),
        |project| CatalogScope::Project {
            id: project.id,
            roots: project.roots.into_iter().map(PathBuf::from).collect(),
        },
    );
    let registry = bundle_admin::existing_registry().await?;
    let disabled = match &registry {
        Some(registry) => registry.disabled_bundle_ids().await?,
        None => BTreeSet::new(),
    };
    let mut installed = Vec::new();
    if let Some(registry) = registry {
        let snapshot = registry.snapshot_with_catalogs().await?;
        for row in snapshot.unreadable {
            warn(&format!(
                "installed bundle {}: {}",
                row.bundle_id, row.error
            ));
        }
        installed.extend(snapshot.bundles.into_iter().map(|loaded| loaded.prepared));
    }
    let roots: Vec<PathBuf> = match &scope {
        CatalogScope::Project { roots, .. } => roots.clone(),
        CatalogScope::Directory(directory) => vec![directory.clone()],
        CatalogScope::Global => Vec::new(),
    };
    let (bundles, warnings) = ProjectScopeRefresh::catalog_for_roots(
        &roots,
        &installed,
        &first_party_catalogs()?,
        &disabled,
    )?;
    for warning in warnings {
        warn(&warning);
    }
    let catalog = AgentCatalog::new(Arc::new(bundles))?;
    let bundle_list: Vec<_> = catalog
        .bundles()
        .bundles()
        .iter()
        .map(|bundle| (**bundle).clone())
        .collect();
    let catalog_roots = WorkflowCatalogRoots::for_scope(Some(workdir), &roots);
    WorkflowCatalog::from_bundles(catalog_roots, &bundle_list)
        .map_err(|error| anyhow::anyhow!("{}: {error}", error.code()))
}
