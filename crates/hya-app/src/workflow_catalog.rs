//! Workflow discovery, source precedence, and immutable catalog views.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_bundle::BundleCatalog;
use hya_engine::{
    AgentCatalog, CatalogScope, CompiledWorkflow, TurnBinding, discover_workflow_files_in_root,
    load_workflow_file, workflow_project_dir, workflow_user_dir,
};
use hya_proto::{
    WorkflowAvailability, WorkflowIdentity, WorkflowInfo, WorkflowRevision, WorkflowSourceId,
    WorkflowStageInfo, WorkflowSummary,
};
use sha2::{Digest, Sha256};

use crate::workflow_control::{WorkflowControlError, bounded_error};
use crate::{ProjectScopeRefresh, bundle_admin, first_party_catalogs};

/// Bundle identity reserved for the read-only first-party Workflow catalog.
pub const FIRST_PARTY_BUNDLE_ID: &str = "hya/plan-impl-review";

const BUNDLE_WORKFLOW_REVISION_DOMAIN: &[u8] = b"hya.workflow.bundle-revision/v1\0";

/// Source tier owning one resolved Workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowCatalogOwner {
    /// Project-local Workflow file.
    Project,
    /// User-level Workflow file.
    User,
    /// Installed Bundle payload.
    Installed {
        /// Owning installed bundle id.
        bundle_id: String,
    },
    /// Read-only first-party Bundle payload.
    FirstParty {
        /// Owning first-party bundle id.
        bundle_id: String,
    },
}

/// Explicit filesystem roots used by one immutable Workflow catalog build.
///
/// `project` lists every Project root's Workflow directory in precedence
/// order (first root wins on a source id or name collision, F2 multi-root).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowCatalogRoots {
    project: Vec<PathBuf>,
    user: Option<PathBuf>,
}

impl WorkflowCatalogRoots {
    /// Construct roots from Workflow directories, not their parent workdirs.
    #[must_use]
    pub fn new(project: Vec<PathBuf>, user: Option<PathBuf>) -> Self {
        Self { project, user }
    }

    /// Derive the production roots for a bound catalog scope and workdir.
    ///
    /// `Global` lists user and bundle rows only (no project root, `hya
    /// serve` has no working directory of its own, ADR-0024).
    /// `Directory(dir)` lists that one directory. `Project { roots, .. }`
    /// lists `workdir` first (it may be the Project root itself, a
    /// registered subdirectory, or the caller's Session workdir), then each
    /// registered root in declaration order, deduped (first root wins).
    #[must_use]
    pub fn for_scope(scope: &CatalogScope, workdir: &Path) -> Self {
        let project = match scope {
            CatalogScope::Global => Vec::new(),
            CatalogScope::Directory(dir) => vec![workflow_project_dir(dir)],
            CatalogScope::Project { roots, .. } => {
                let mut seen = BTreeSet::new();
                let mut project = Vec::new();
                for candidate in std::iter::once(workdir).chain(roots.iter().map(PathBuf::as_path))
                {
                    let root = workflow_project_dir(candidate);
                    if seen.insert(root.clone()) {
                        project.push(root);
                    }
                }
                project
            }
        };
        Self {
            project,
            user: workflow_user_dir(),
        }
    }
}

/// One immutable Workflow plus its exact identity and source owner.
pub struct ResolvedWorkflow {
    identity: WorkflowIdentity,
    workflow: Arc<CompiledWorkflow>,
    display_path: String,
    owner: WorkflowCatalogOwner,
}

impl ResolvedWorkflow {
    /// Return the exact source/name/revision identity.
    #[must_use]
    pub fn identity(&self) -> &WorkflowIdentity {
        &self.identity
    }

    /// Return the validated compiled Workflow.
    #[must_use]
    pub fn workflow(&self) -> &CompiledWorkflow {
        self.workflow.as_ref()
    }

    /// Return the path or package source label used for diagnostics.
    #[must_use]
    pub fn display_path(&self) -> &str {
        &self.display_path
    }

    /// Return the source tier that owns this Workflow.
    #[must_use]
    pub fn owner(&self) -> &WorkflowCatalogOwner {
        &self.owner
    }
}

/// One invalid filesystem source retained for list diagnostics and stale state.
struct InvalidWorkflowSource {
    name: String,
    source: WorkflowSourceId,
    path: String,
    error: String,
}

/// One immutable valid/invalid row in catalog precedence order.
enum WorkflowCatalogRow {
    Valid(usize),
    Invalid(InvalidWorkflowSource),
}

/// One resolved filesystem Workflow row, not yet admitted to a catalog.
enum FilesystemRow {
    Valid(ResolvedWorkflow),
    Invalid(InvalidWorkflowSource),
}

/// Resolve one filesystem path below `root` to its source id and row.
///
/// `tier` is `"project"` or `"user"`: a project source id is the path
/// relative to its own root (so the same root produces the same id across
/// process restarts); a user source id is the absolute path (there is only
/// ever one user root).
fn filesystem_row(path: PathBuf, root: &Path, tier: &str) -> (String, FilesystemRow) {
    let relative = path.strip_prefix(root).map_or_else(
        |_| path.display().to_string(),
        |value| value.display().to_string(),
    );
    let source_value = if tier == "project" {
        relative
    } else {
        path.display().to_string()
    };
    let source = WorkflowSourceId::new(format!("{tier}:{source_value}"));
    let source_id = source.as_str().to_string();
    let row = match load_workflow_file(&path) {
        Ok(workflow) => FilesystemRow::Valid(ResolvedWorkflow {
            identity: WorkflowIdentity {
                source,
                name: workflow.definition().name().to_string(),
                revision: workflow.revision(),
            },
            workflow: Arc::new(workflow),
            display_path: path.display().to_string(),
            owner: if tier == "project" {
                WorkflowCatalogOwner::Project
            } else {
                WorkflowCatalogOwner::User
            },
        }),
        Err(error) => FilesystemRow::Invalid(InvalidWorkflowSource {
            name: source_stem(&path),
            source,
            path: path.display().to_string(),
            error: bounded_error(&error.to_string()),
        }),
    };
    (source_id, row)
}

/// Immutable resolver over project, user, installed, and first-party sources.
pub struct WorkflowCatalog {
    entries: Vec<ResolvedWorkflow>,
    rows: Vec<WorkflowCatalogRow>,
}

impl WorkflowCatalog {
    /// Inspect prepared Workflow sources for a working directory without starting a runtime.
    ///
    /// An existing database is opened read-only to resolve Project roots. Bundle
    /// diagnostics are delivered in discovery order through `warn`.
    ///
    /// # Errors
    /// Returns database, bundle discovery, or catalog construction errors.
    pub async fn inspect_read_only(
        db: &str,
        workdir: &Path,
        mut warn: impl FnMut(&str),
    ) -> anyhow::Result<Self> {
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
        // `use`/`run` register an unregistered directory as a one-root Project,
        // so its own `.hya/bundles` belong to the catalog here too.
        let roots = match &scope {
            CatalogScope::Project { roots, .. } => roots.as_slice(),
            CatalogScope::Directory(directory) => std::slice::from_ref(directory),
            CatalogScope::Global => &[],
        };
        let (bundles, warnings) = ProjectScopeRefresh::catalog_for_roots(
            roots,
            &installed,
            &first_party_catalogs()?,
            &disabled,
        )?;
        for warning in warnings {
            warn(&warning);
        }
        let bundles = AgentCatalog::new(Arc::new(bundles))?;
        Self::from_bundles(
            WorkflowCatalogRoots::for_scope(&scope, workdir),
            bundles.bundles(),
        )
        .map_err(|error| anyhow::anyhow!("{}: {error}", error.code()))
    }

    /// Build one catalog from explicit filesystem roots and a pinned runtime.
    ///
    /// The binding supplies the exact installed/first-party bundle snapshot;
    /// catalog construction never consults a newer runtime generation.
    ///
    /// # Errors
    /// Returns a control error when a prepared bundle Workflow cannot compile or
    /// disagrees with its prepared identity.
    pub fn build(
        roots: WorkflowCatalogRoots,
        binding: &TurnBinding,
    ) -> Result<Self, WorkflowControlError> {
        Self::from_bundles(roots, binding.bundle_catalog())
    }

    fn from_bundles(
        roots: WorkflowCatalogRoots,
        bundles: &BundleCatalog,
    ) -> Result<Self, WorkflowControlError> {
        let mut catalog = Self {
            entries: Vec::new(),
            rows: Vec::new(),
        };
        catalog.append_project_roots(&roots.project);
        if let Some(user) = roots.user.as_deref() {
            catalog.append_filesystem_root(user, "user");
        }
        for bundle in bundles.bundles() {
            let Some(prepared) = bundle.workflow() else {
                continue;
            };
            let source = format!("bundle:{}/workflow/{}", bundle.identity().id, prepared.id);
            let workflow =
                hya_workflow::compile(hya_workflow::WorkflowSource::new(&source, &prepared.source))
                    .map_err(|error| {
                        WorkflowControlError::InvalidSource(bounded_error(&error.to_string()))
                    })?;
            if workflow.definition().name() != prepared.id {
                return Err(WorkflowControlError::InvalidSource(format!(
                    "prepared Workflow `{}` declares `{}`",
                    prepared.id,
                    workflow.definition().name()
                )));
            }
            let compiler_revision = workflow.revision().to_string();
            if prepared.compiler_revision != compiler_revision {
                return Err(WorkflowControlError::InvalidSource(format!(
                    "prepared Workflow `{}` compiler revision does not match current compiler",
                    prepared.id
                )));
            }
            let revision = bundle_workflow_revision(&workflow, bundle.digest());
            let owner = if bundle.identity().id == FIRST_PARTY_BUNDLE_ID {
                WorkflowCatalogOwner::FirstParty {
                    bundle_id: bundle.identity().id.clone(),
                }
            } else {
                WorkflowCatalogOwner::Installed {
                    bundle_id: bundle.identity().id.clone(),
                }
            };
            let entry = ResolvedWorkflow {
                identity: WorkflowIdentity {
                    source: WorkflowSourceId::new(source),
                    name: prepared.id.clone(),
                    revision,
                },
                workflow: Arc::new(workflow),
                display_path: format!("{}:{}", bundle.identity().id, prepared.source_path),
                owner,
            };
            catalog.entries.push(entry);
            catalog
                .rows
                .push(WorkflowCatalogRow::Valid(catalog.entries.len() - 1));
        }
        Ok(catalog)
    }

    /// Return the effective list in source precedence order.
    #[must_use]
    pub fn list(&self) -> Vec<WorkflowSummary> {
        let mut seen = BTreeSet::new();
        let mut summaries = Vec::new();
        for row in &self.rows {
            match row {
                WorkflowCatalogRow::Valid(index) => {
                    let entry = &self.entries[*index];
                    if !seen.insert(entry.identity.name.clone()) {
                        continue;
                    }
                    summaries.push(WorkflowSummary {
                        name: entry.identity.name.clone(),
                        description: entry.workflow.definition().description().to_string(),
                        source: Some(entry.identity.source.clone()),
                        revision: Some(entry.identity.revision),
                        stages: entry
                            .workflow
                            .plan()
                            .stages()
                            .iter()
                            .map(|stage| stage.id().to_string())
                            .collect(),
                        path: entry.display_path.clone(),
                        error: None,
                    });
                }
                WorkflowCatalogRow::Invalid(invalid) => summaries.push(WorkflowSummary {
                    name: invalid.name.clone(),
                    description: String::new(),
                    source: None,
                    revision: None,
                    stages: Vec::new(),
                    path: invalid.path.clone(),
                    error: Some(invalid.error.clone()),
                }),
            }
        }
        summaries
    }

    /// Resolve a qualified source or an effective bare Workflow name.
    ///
    /// Project/user names shadow bundles. A bare bundle name resolves only when
    /// exactly one bundle owns that name; qualified bundle ids are exact.
    ///
    /// # Errors
    /// Returns [`WorkflowControlError::NotFound`] for an unavailable name and
    /// [`WorkflowControlError::InvalidSource`] for a matching broken file.
    pub fn resolve(&self, reference: &str) -> Result<&ResolvedWorkflow, WorkflowControlError> {
        if reference.starts_with("bundle:")
            || reference.starts_with("project:")
            || reference.starts_with("user:")
        {
            for row in &self.rows {
                match row {
                    WorkflowCatalogRow::Valid(index)
                        if self.entries[*index].identity.source.as_str() == reference =>
                    {
                        return Ok(&self.entries[*index]);
                    }
                    WorkflowCatalogRow::Invalid(invalid)
                        if invalid.source.as_str() == reference =>
                    {
                        return Err(WorkflowControlError::InvalidSource(invalid.error.clone()));
                    }
                    _ => {}
                }
            }
            return Err(WorkflowControlError::NotFound {
                name: reference.to_string(),
            });
        }

        let mut bundle_match = None;
        for row in &self.rows {
            let WorkflowCatalogRow::Valid(index) = row else {
                continue;
            };
            let entry = &self.entries[*index];
            if entry.identity.name != reference {
                continue;
            }
            if matches!(
                &entry.owner,
                WorkflowCatalogOwner::Project | WorkflowCatalogOwner::User
            ) {
                return Ok(entry);
            }
            if bundle_match.is_some() {
                bundle_match = None;
                break;
            }
            bundle_match = Some(entry);
        }
        if let Some(entry) = bundle_match {
            return Ok(entry);
        }
        for row in &self.rows {
            if let WorkflowCatalogRow::Invalid(invalid) = row
                && invalid.name == reference
            {
                return Err(WorkflowControlError::InvalidSource(invalid.error.clone()));
            }
        }
        Err(WorkflowControlError::NotFound {
            name: reference.to_string(),
        })
    }

    /// Resolve one Workflow and project its immutable compiled metadata.
    ///
    /// # Errors
    /// Propagates the exact resolution error from [`Self::resolve`].
    pub fn info(&self, reference: &str) -> Result<WorkflowInfo, WorkflowControlError> {
        let entry = self.resolve(reference)?;
        Ok(workflow_info(entry))
    }

    /// Compare one persisted identity with this exact catalog snapshot.
    #[must_use]
    pub fn availability(&self, selection: &WorkflowIdentity) -> WorkflowAvailability {
        for row in &self.rows {
            match row {
                WorkflowCatalogRow::Valid(index)
                    if self.entries[*index].identity.source == selection.source =>
                {
                    return if &self.entries[*index].identity == selection {
                        WorkflowAvailability::Available
                    } else {
                        WorkflowAvailability::Stale
                    };
                }
                WorkflowCatalogRow::Invalid(invalid) if invalid.source == selection.source => {
                    return WorkflowAvailability::Stale;
                }
                _ => {}
            }
        }
        WorkflowAvailability::Unavailable
    }

    /// Append valid and invalid filesystem rows below one explicit root.
    fn append_filesystem_root(&mut self, root: &Path, tier: &str) {
        for path in discover_workflow_files_in_root(root) {
            let (_, row) = filesystem_row(path, root, tier);
            self.push_filesystem_row(row);
        }
    }

    /// Append filesystem rows below every Project root, in order.
    ///
    /// Every root's Workflow directory is discovered; a later root's file
    /// whose `project:<path relative to its root>` source id or compiled
    /// Workflow name collides with an earlier root's is skipped (first root
    /// wins) with a warning. A later root's invalid source with a distinct
    /// id is always kept: it is still worth surfacing as a diagnostic row.
    fn append_project_roots(&mut self, roots: &[PathBuf]) {
        let mut seen_source_ids = BTreeSet::new();
        let mut seen_names = BTreeSet::new();
        for root in roots {
            for path in discover_workflow_files_in_root(root) {
                let (source_id, row) = filesystem_row(path, root, "project");
                if !seen_source_ids.insert(source_id.clone()) {
                    tracing::warn!(
                        root = %root.display(),
                        source = %source_id,
                        "skipped a project Workflow source: another root already publishes this source id"
                    );
                    continue;
                }
                if let FilesystemRow::Valid(entry) = &row
                    && !seen_names.insert(entry.identity.name.clone())
                {
                    tracing::warn!(
                        root = %root.display(),
                        name = %entry.identity.name,
                        source = %source_id,
                        "skipped a project Workflow source: another root already publishes this name"
                    );
                    continue;
                }
                self.push_filesystem_row(row);
            }
        }
    }

    /// Push one already-resolved filesystem row into the catalog.
    fn push_filesystem_row(&mut self, row: FilesystemRow) {
        match row {
            FilesystemRow::Valid(entry) => {
                self.entries.push(entry);
                self.rows
                    .push(WorkflowCatalogRow::Valid(self.entries.len() - 1));
            }
            FilesystemRow::Invalid(invalid) => self.rows.push(WorkflowCatalogRow::Invalid(invalid)),
        }
    }
}

/// Fold one bundle digest into a compiled Workflow revision.
fn bundle_workflow_revision(workflow: &CompiledWorkflow, bundle_digest: &str) -> WorkflowRevision {
    let mut hasher = Sha256::new();
    hasher.update(BUNDLE_WORKFLOW_REVISION_DOMAIN);
    hasher.update(workflow.revision().as_bytes());
    hasher.update(bundle_digest.as_bytes());
    WorkflowRevision::from_bytes(hasher.finalize().into())
}

/// Convert one immutable resolved Workflow to the shared info DTO.
fn workflow_info(entry: &ResolvedWorkflow) -> WorkflowInfo {
    let plan = entry.workflow().plan();
    WorkflowInfo {
        identity: entry.identity().clone(),
        description: entry.workflow().definition().description().to_string(),
        inputs: entry.workflow().definition().inputs().clone(),
        on_failure: entry.workflow().definition().on_failure().to_string(),
        stages: plan
            .stages()
            .iter()
            .map(|stage| WorkflowStageInfo {
                id: stage.id().to_string(),
                title: stage.title().map(str::to_string),
                agent: stage.agent().to_string(),
                level: stage.level(),
                predecessors: stage
                    .predecessor_indices()
                    .iter()
                    .map(|&index| plan.stages()[index].id().to_string())
                    .collect(),
                actor: stage.actor().map(str::to_string),
                mode: stage.mode().to_string(),
                worker_model: stage.model().cloned(),
                verifier_model: stage.verify().and_then(|verify| verify.model()).cloned(),
            })
            .collect(),
        path: entry.display_path().to_string(),
    }
}

/// Strip the complete Workflow suffix for an invalid-source discovery row.
fn source_stem(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".hya.md"))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn process_command_changes_packaged_workflow_revision() {
        use hya_bundle::{BundleSource, SourceFile, prepare_package};
        let workflow_source = "---\nkind: Workflow\nname: demo\ndescription: Digest revision regression.\nnodes:\n  execute:\n    agent: worker\n    directive: Execute.\n---\nflowchart TD\n  execute\n";
        let prepare = |command: &str| {
            let manifest = format!(
                "kind: Bundle\nidentity: {{id: hya/demo, version: 1.0.0, publisher: hya}}\nagents: [{{id: worker, role: subagent}}]\nworkflow: {{id: demo, path: workflows/demo.hya.md}}\nextensions:\n  process: {{kind: rust, command: [{command}]}}\n"
            );
            prepare_package(BundleSource::new(
                "revision",
                vec![
                    SourceFile::new("bundle.yaml", manifest.into_bytes()),
                    SourceFile::new("workflows/demo.hya.md", workflow_source.as_bytes()),
                ],
            ))
            .unwrap_or_else(|error| panic!("prepare process-backed Workflow: {error}"))
        };
        let before = prepare("provider-before");
        let after = prepare("provider-after");
        let compiled = hya_workflow::compile(hya_workflow::WorkflowSource::new(
            "workflows/demo.hya.md",
            workflow_source,
        ))
        .unwrap_or_else(|error| panic!("compile Workflow: {error}"));
        assert_ne!(before.bundles()[0].digest(), after.bundles()[0].digest());
        assert_ne!(
            super::bundle_workflow_revision(&compiled, before.bundles()[0].digest()),
            super::bundle_workflow_revision(&compiled, after.bundles()[0].digest()),
        );
    }
}
