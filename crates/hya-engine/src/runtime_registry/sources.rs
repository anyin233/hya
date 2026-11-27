use super::*;

mod claims;

use super::fingerprint::runtime_source_dispatch_identity;

/// Offline mutable candidate. Its contents cannot become effective except
/// through [`RuntimeRegistry::refresh`].
pub struct RuntimeCandidate {
    pub(super) catalog: Arc<AgentCatalog>,
    pub(super) discover_skills: Option<SkillDiscover>,
    pub(super) tools: ToolRegistry,
    pub(super) skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
    pub(super) sources: BTreeMap<RuntimeSourceId, RuntimeSource>,
    pub(super) masks: BTreeMap<String, String>,
    pub(super) schemes: BTreeMap<String, SchemeBinding>,
}

/// Kind of runtime contribution source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeSourceKind {
    /// Prepared bundle resources and their generation-owned process providers.
    Bundle,
    /// MCP server tools.
    Mcp,
    /// Plugin-declared tools and Skills.
    Plugin,
}

/// Stable identity of a configured bundle, MCP, or plugin source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeSourceId {
    kind: RuntimeSourceKind,
    configured_id: String,
}

/// Connection owner retained by a published runtime source.
/// Shutdown stops and joins connection tasks even while bindings retain the source.
pub trait RuntimeSourceOwner: Send + Sync {
    /// Static sources have no connection work to stop.
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
}

impl RuntimeSourceOwner for () {}

/// One tool export from a runtime source (canonical name + aliases).
#[derive(Clone)]
pub struct RuntimeSourceExport {
    pub(super) declared_id: String,
    pub(super) canonical_name: String,
    pub(super) aliases: Vec<String>,
    pub(super) tool: Arc<dyn Tool>,
    pub(super) permission: ToolPermission,
}

/// Published bundle/MCP/plugin source with tools, Skills, and opaque resources.
#[derive(Clone)]
pub struct RuntimeSource {
    pub(super) id: RuntimeSourceId,
    pub(super) project_plugin: bool,
    pub(super) declaration_digest: [u8; 32],
    pub(super) owner: Arc<dyn RuntimeSourceOwner>,
    pub(super) exports: Vec<RuntimeSourceExport>,
    pub(super) skills: Vec<RuntimeSourceSkill>,
    pub(super) resources: Arc<BTreeMap<String, Value>>,
    pub(super) schemas: Vec<SourceSchema>,
    pub(super) hooks: Option<Arc<dyn crate::hooks::HookDispatcher>>,
    pub(super) apis: Option<crate::bundle_apis::SourceApis>,
    pub(super) permission_modes: Vec<crate::permission_mode::RuntimePermissionMode>,
}

/// One external URI-scheme claim a runtime source makes.
///
/// `canonical_tool` must be a canonical export of the *same* source — except
/// for `Bundle` sources, whose tools are view-scoped (sidecar activation)
/// rather than registry exports, so a bundle claims its tool by the
/// `bundle:{id}/tool/{local}` stable id even though it exports no registry
/// tools. `scheme` must be a publishable token; both rules are enforced at
/// publication. Claims are adjudicated across sources the way bare-name
/// aliases are: the lexicographically greater source id wins and the table
/// records only the winner, with the full claim chain still derivable through
/// [`TurnBinding::scheme_chain`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSchema {
    /// The external URI scheme (e.g. `db` for `db://x/y`).
    pub scheme: String,
    /// Canonical name of this source's tool that serves the scheme.
    pub canonical_tool: String,
    /// Whether the scheme accepts write dispatch in addition to reads.
    pub writable: bool,
}

/// One Skill contribution materialized for a published runtime source.
///
/// `stable_id` is the resource identity used by bundle resource views. The
/// parsed entry carries the complete Skill metadata and a deterministic virtual
/// path; no runtime consumer reparses the original Markdown.
#[derive(Clone)]
pub struct RuntimeSourceSkill {
    pub(super) stable_id: String,
    pub(super) local_id: String,
    pub(super) aliases: Vec<String>,
    pub(super) digest: String,
    pub(super) content: String,
    pub(super) entry: SkillCatalogEntry,
}

/// Serialisable summary of a source for diagnostics/UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSourceManifest {
    /// Source identity.
    pub id: RuntimeSourceId,
    /// Digest of the source declaration.
    pub declaration_digest: [u8; 32],
    /// Canonical export names.
    pub exports: Vec<String>,
    /// Canonical Skill resource ids.
    pub skills: Vec<String>,
    /// Parsed Skill metadata retained by the published source.
    pub skill_entries: Vec<SkillCatalogEntry>,
    /// Opaque resource map from the source.
    pub resources: Arc<BTreeMap<String, Value>>,
}

impl RuntimeCandidate {
    pub(super) fn into_snapshot(
        self,
        generation: ConfigGeneration,
        basic_tools: ToolRegistrySnapshot,
    ) -> RuntimeSnapshot {
        let Self {
            catalog,
            tools,
            skills,
            discover_skills: _,
            sources,
            masks,
            schemes,
        } = self;
        RuntimeSnapshot {
            generation,
            catalog,
            basic_tools,
            tools: tools.snapshot(),
            skills,
            sources,
            masks: Arc::new(masks),
            schemes: Arc::new(schemes),
        }
    }

    pub(super) fn from_snapshot(snapshot: &RuntimeSnapshot) -> Self {
        Self {
            catalog: Arc::clone(&snapshot.catalog),
            discover_skills: None,
            tools: ToolRegistry::from_snapshot(&snapshot.tools),
            skills: snapshot.skills.clone(),
            sources: snapshot.sources.clone(),
            masks: snapshot.masks.as_ref().clone(),
            schemes: snapshot.schemes.as_ref().clone(),
        }
    }

    /// Replace the complete Agent/Bundle catalog on this offline candidate.
    pub fn replace_catalog(&mut self, catalog: Arc<AgentCatalog>) {
        self.catalog = catalog;
    }

    /// Register a tool with default `Tool` permission on this candidate.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), RuntimeRefreshError> {
        self.register_tool_with_permission(tool, ToolPermission::Tool)
    }

    /// Register a tool with an explicit permission class on this candidate.
    pub fn register_tool_with_permission(
        &mut self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), RuntimeRefreshError> {
        self.tools.register_with_permission(tool, permission)?;
        Ok(())
    }

    /// Remove a tool and its aliases from this candidate.
    pub fn remove_tool(&mut self, name: &str) {
        if self.tools.resolve(name).is_some() {
            self.tools.remove(name);
        }
    }

    /// Rediscover skills for `workdir` through the registry's configured source.
    pub fn refresh_skills(&mut self, workdir: &Path) {
        if let Some(discover) = self.discover_skills {
            self.replace_skills(workdir, discover(Some(workdir), &[]).skills);
        }
    }

    /// Insert or replace MCP/plugin sources on this candidate.
    pub fn upsert_sources(
        &mut self,
        sources: Vec<RuntimeSource>,
    ) -> Result<(), RuntimeRefreshError> {
        let mut ids = BTreeSet::new();
        for source in &sources {
            if !ids.insert(source.id.clone()) {
                return Err(RuntimeRefreshError::InvalidCandidate(format!(
                    "duplicate runtime source {}",
                    source.id
                )));
            }
        }
        self.validate_candidate_source_names(&sources)
            .map(|masks| {
                self.masks = masks;
            })?;
        self.schemes = self.validate_candidate_source_schemes(&sources)?;

        for source in &sources {
            if let Some(previous) = self.sources.remove(&source.id) {
                for export in previous.exports {
                    self.tools.remove(&export.canonical_name);
                }
            }
        }
        for source in sources {
            let mut declared = BTreeSet::new();
            for export in &source.exports {
                if !declared.insert(export.declared_id.as_str()) {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "duplicate export {} for source {}",
                        export.declared_id, source.id
                    )));
                }
                if export.tool.name() != export.canonical_name {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "export {} canonical name {} does not match tool name {}",
                        export.declared_id,
                        export.canonical_name,
                        export.tool.name()
                    )));
                }
                // Contested aliases are never registered on the tool plane;
                // the bare name resolves through the snapshot mask table and
                // compiled views instead.
                let registered_aliases = export
                    .aliases
                    .iter()
                    .filter(|alias| !self.masks.contains_key(alias.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                let identity = runtime_source_dispatch_identity(&source, export)?;
                self.tools
                    .register_with_permission_and_aliases_and_dispatch_identity(
                        export.tool.clone(),
                        export.permission,
                        &registered_aliases,
                        identity,
                    )?;
            }
            let mut skill_ids = BTreeSet::new();
            for skill in &source.skills {
                if !skill_ids.insert(skill.stable_id.as_str()) {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "duplicate Skill {} for source {}",
                        skill.stable_id, source.id
                    )));
                }
                if skill.local_id.is_empty() || skill.entry.name != skill.local_id {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "Skill {} local id does not match parsed name for source {}",
                        skill.stable_id, source.id
                    )));
                }
                match source.id.kind {
                    RuntimeSourceKind::Bundle => {
                        let Some(bundle_path) = skill.stable_id.strip_prefix("bundle:") else {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "bundle source {} published non-bundle Skill {}",
                                source.id, skill.stable_id
                            )));
                        };
                        let expected =
                            format!("{}/skill/{}", source.id.configured_id, skill.local_id);
                        if bundle_path != expected {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "bundle Skill {} is not owned by source {}",
                                skill.stable_id, source.id
                            )));
                        }
                    }
                    RuntimeSourceKind::Mcp | RuntimeSourceKind::Plugin => {
                        if skill.stable_id.starts_with("bundle:") {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "non-bundle source {} cannot publish bundle Skill {}",
                                source.id, skill.stable_id
                            )));
                        }
                    }
                }
            }
            self.sources.insert(source.id.clone(), source);
        }
        Ok(())
    }

    /// Replace every source of one kind while preserving other source kinds.
    pub fn replace_sources_of_kind(
        &mut self,
        kind: RuntimeSourceKind,
        sources: Vec<RuntimeSource>,
    ) -> Result<(), RuntimeRefreshError> {
        if let Some(source) = sources.iter().find(|source| source.id.kind() != kind) {
            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                "source {} does not belong to replacement kind {kind:?}",
                source.id
            )));
        }
        let removed = self
            .sources
            .keys()
            .filter(|id| id.kind() == kind)
            .cloned()
            .collect::<BTreeSet<_>>();
        self.remove_sources(&removed);
        self.upsert_sources(sources)
    }

    /// Remove sources by id from this candidate.
    ///
    /// The mask table is re-derived from the retained sources, so masks whose
    /// winner was removed dissolve. Note that a losing source's contested
    /// alias was never registered on the tool plane; it becomes resolvable
    /// again through the bare plane only after that source re-publishes, while
    /// its qualified canonical spelling always stays available.
    pub fn remove_sources(&mut self, removed: &BTreeSet<RuntimeSourceId>) {
        for id in removed {
            if let Some(source) = self.sources.remove(id) {
                for export in source.exports {
                    self.tools.remove(&export.canonical_name);
                }
            }
        }
        self.recompute_masks_after_removal();
        self.recompute_schemes_after_removal();
    }

    pub(super) fn replace_skills(&mut self, workdir: &Path, skills: Vec<SkillCatalogEntry>) {
        let existing = self
            .skills
            .get(workdir)
            .map_or(&[][..], |current| current.as_slice());
        if existing == skills {
            return;
        }
        if skills.is_empty() {
            self.skills.remove(workdir);
        } else {
            self.skills.insert(workdir.to_path_buf(), Arc::new(skills));
        }
    }

    pub(super) fn logically_matches(&self, snapshot: &RuntimeSnapshot) -> bool {
        self.catalog.bundles().bundles() == snapshot.catalog.bundles().bundles()
            && self.tools.logically_matches(&snapshot.tools)
            && self.skills == snapshot.skills
            && sources_match(&self.sources, &snapshot.sources)
    }
}

impl RuntimeSourceId {
    #[must_use]
    /// Build a source id from kind and configured identifier.
    pub fn new(kind: RuntimeSourceKind, configured_id: impl Into<String>) -> Self {
        Self {
            kind,
            configured_id: configured_id.into(),
        }
    }

    #[must_use]
    /// Construct a statically prepared bundle [`RuntimeSourceId`].
    pub fn bundle(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Bundle, configured_id)
    }

    #[must_use]
    /// Construct an MCP [`RuntimeSourceId`].
    pub fn mcp(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Mcp, configured_id)
    }

    #[must_use]
    /// Construct a plugin [`RuntimeSourceId`].
    pub fn plugin(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Plugin, configured_id)
    }

    #[must_use]
    /// Return the source kind.
    pub fn kind(&self) -> RuntimeSourceKind {
        self.kind
    }

    #[must_use]
    /// Return the configured id string.
    pub fn configured_id(&self) -> &str {
        &self.configured_id
    }
}

impl std::fmt::Display for RuntimeSourceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            RuntimeSourceKind::Bundle => "bundle",
            RuntimeSourceKind::Mcp => "mcp",
            RuntimeSourceKind::Plugin => "plugin",
        };
        write!(formatter, "{kind}:{}", self.configured_id)
    }
}

impl RuntimeSourceExport {
    #[must_use]
    /// Build one export describing a tool and its aliases.
    pub fn tool(
        declared_id: impl Into<String>,
        canonical_name: impl Into<String>,
        aliases: Vec<String>,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Self {
        Self {
            declared_id: declared_id.into(),
            canonical_name: canonical_name.into(),
            aliases,
            tool,
            permission,
        }
    }
}
impl RuntimeSourceSkill {
    /// Build one source-owned Skill entry from a parsed contribution.
    #[must_use]
    pub fn new(
        stable_id: impl Into<String>,
        local_id: impl Into<String>,
        aliases: Vec<String>,
        digest: impl Into<String>,
        content: impl Into<String>,
        entry: SkillCatalogEntry,
    ) -> Self {
        Self {
            stable_id: stable_id.into(),
            local_id: local_id.into(),
            aliases,
            digest: digest.into(),
            content: content.into(),
            entry,
        }
    }

    /// Stable resource id used in bundle resource views.
    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    /// Bundle or plugin-local Skill id.
    #[must_use]
    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Declared SHA-256 digest of the complete Skill content.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Complete Skill Markdown declaration bytes.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Parsed Skill metadata retained by the source.
    #[must_use]
    pub fn entry(&self) -> &SkillCatalogEntry {
        &self.entry
    }
}

impl RuntimeSource {
    /// Build a published source with exports, Skills, and an empty resource map.
    #[must_use]
    pub fn new(
        id: RuntimeSourceId,
        declaration_digest: [u8; 32],
        owner: Arc<dyn RuntimeSourceOwner>,
        exports: Vec<RuntimeSourceExport>,
    ) -> Self {
        Self {
            id,
            declaration_digest,
            owner,
            exports,
            skills: Vec::new(),
            resources: Arc::new(BTreeMap::new()),
            schemas: Vec::new(),
            hooks: None,
            apis: None,
            permission_modes: Vec::new(),
            project_plugin: false,
        }
    }

    /// The API endpoints attached with [`Self::with_apis`], if any.
    #[must_use]
    pub fn apis(&self) -> Option<&crate::bundle_apis::SourceApis> {
        self.apis.as_ref()
    }

    /// Attach the API endpoints this source's process serves.
    ///
    /// The provider is retained together with the source owner, so a request
    /// that resolved this generation keeps its process alive until it
    /// completes even if a newer generation is published meanwhile. An empty
    /// `apis` list attaches nothing.
    #[must_use]
    pub fn with_apis(
        mut self,
        mut apis: Vec<crate::bundle_apis::SourceApi>,
        provider: Arc<dyn crate::bundle_apis::BundleApiProvider>,
    ) -> Self {
        if apis.is_empty() {
            self.apis = None;
            return self;
        }
        apis.sort_by(|left, right| left.id.cmp(&right.id));
        self.apis = Some(crate::bundle_apis::SourceApis {
            apis,
            provider,
            _owner: Some(Arc::clone(&self.owner)),
        });
        self
    }

    /// Attach API endpoints backed by native ABI v2
    /// [`hya_runtime_abi::NativeBundleApiHandler`] instances.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] if any declared native endpoint path
    /// template is invalid or duplicated.
    pub fn with_native_apis(
        mut self,
        handlers: Vec<Arc<dyn hya_runtime_abi::NativeBundleApiHandler>>,
    ) -> Result<Self, crate::CoreError> {
        let (apis, provider) = crate::bundle_apis::source_apis_from_native_handlers(
            self.id.configured_id(),
            handlers,
        )?;
        self = self.with_apis(apis, provider);
        Ok(self)
    }

    /// Attach the session permission modes this source's process approves
    /// through its `permission.approve` hook (sorted by id).
    #[must_use]
    pub fn with_permission_modes(
        mut self,
        mut modes: Vec<crate::permission_mode::RuntimePermissionMode>,
    ) -> Self {
        modes.sort_by(|left, right| left.id.cmp(&right.id));
        self.permission_modes = modes;
        self
    }

    /// The session permission modes this source declares.
    #[must_use]
    pub fn permission_modes(&self) -> &[crate::permission_mode::RuntimePermissionMode] {
        &self.permission_modes
    }

    /// Attach process hooks retained by this immutable runtime generation.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn crate::hooks::HookDispatcher>) -> Self {
        self.hooks = Some(Arc::new(crate::bundle_hooks::ScopedBundleHooks::retaining(
            hooks,
            Arc::clone(&self.owner),
        )));
        self
    }

    /// Attach parsed Skill contributions to the source.
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<RuntimeSourceSkill>) -> Self {
        self.skills = skills;
        self
    }

    /// Attach external URI-scheme claims to the source.
    #[must_use]
    pub fn with_schemas(mut self, schemas: Vec<SourceSchema>) -> Self {
        self.schemas = schemas;
        self
    }

    #[must_use]
    /// Attach opaque JSON resources to the source.
    pub fn with_resources(mut self, resources: BTreeMap<String, Value>) -> Self {
        self.resources = Arc::new(resources);
        self
    }

    #[must_use]
    /// Borrow the source identifier.
    pub fn id(&self) -> &RuntimeSourceId {
        &self.id
    }

    #[must_use]
    /// The external URI-scheme claims this source makes.
    pub fn schemas(&self) -> &[SourceSchema] {
        &self.schemas
    }
}

fn sources_match(
    left: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    right: &BTreeMap<RuntimeSourceId, RuntimeSource>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, left)| {
            right.get(id).is_some_and(|right| {
                left.declaration_digest == right.declaration_digest
                    && Arc::ptr_eq(&left.owner, &right.owner)
                    && match (&left.hooks, &right.hooks) {
                        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                        (None, None) => true,
                        _ => false,
                    }
                    && match (&left.apis, &right.apis) {
                        (Some(left), Some(right)) => {
                            left.apis == right.apis && Arc::ptr_eq(&left.provider, &right.provider)
                        }
                        (None, None) => true,
                        _ => false,
                    }
                    && left.resources == right.resources
                    && left.permission_modes == right.permission_modes
                    && left.schemas == right.schemas
                    && left.skills.len() == right.skills.len()
                    && left.skills.iter().zip(&right.skills).all(|(left, right)| {
                        left.stable_id == right.stable_id
                            && left.local_id == right.local_id
                            && left.aliases == right.aliases
                            && left.digest == right.digest
                            && left.content == right.content
                            && left.entry == right.entry
                    })
                    && left.exports.len() == right.exports.len()
                    && left
                        .exports
                        .iter()
                        .zip(&right.exports)
                        .all(|(left, right)| {
                            left.declared_id == right.declared_id
                                && left.canonical_name == right.canonical_name
                                && left.aliases == right.aliases
                                && left.permission == right.permission
                                && Arc::ptr_eq(&left.tool, &right.tool)
                        })
            })
        })
}
