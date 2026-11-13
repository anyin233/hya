use super::*;
const RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.core.runtime-source-dispatch/v1";
pub(super) const RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2: &[u8] =
    b"hya.core.runtime-semantic-fingerprint/v2";

/// Offline mutable candidate. Its contents cannot become effective except
/// through [`RuntimeRegistry::refresh`].
pub struct RuntimeCandidate {
    pub(super) catalog: Arc<AgentCatalog>,
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

    /// Rediscover skills for `workdir` into this candidate.
    pub fn refresh_skills(&mut self, workdir: &Path) {
        self.replace_skills(workdir, discover_skills_with_builtins(workdir));
    }

    /// Validate every contributed source export name against the naming rules
    /// before any mutation happens, and derive the bare-name mask table:
    ///
    /// - plugin sources must publish qualified `namespace__local` tool names
    ///   (bare names are reserved for built-in tools) whose namespace head is
    ///   not one of the reserved `mcp`/`harness`/`builtin` planes;
    /// - MCP sources must publish exactly `mcp__<server>__<local>` with valid
    ///   tokens;
    /// - canonical names must be unique across sources (hard error);
    /// - the protected bare name `read` can never be claimed or masked by a
    ///   contributed alias (hard error);
    /// - any other alias collision — with a built-in canonical name, another
    ///   source's canonical name, or another source's alias — becomes an
    ///   explicit mask instead of an error. A source always beats a built-in
    ///   (user scope over the built-in plane), and between two sources the
    ///   lexicographically GREATER source id string wins, which approximates
    ///   "the newer install wins". The winner's tool provides the contested
    ///   name; the loser keeps its qualified canonical spelling as the escape
    ///   hatch.
    ///
    /// All hard conflicts are reported in one grouped diagnostic. The returned
    /// map is the published mask table (contested bare name → winning
    /// canonical name); contested aliases are deliberately not registered as
    /// registry aliases, so the bare plane resolves through compiled views.
    fn validate_candidate_source_names(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, String>, RuntimeRefreshError> {
        let (conflicts, masks) = self.adjudicate_source_name_claims(sources);
        if conflicts.is_empty() {
            Ok(masks)
        } else {
            Err(RuntimeRefreshError::NamingConflicts(conflicts.join("\n")))
        }
    }

    /// One claim on a name in the global bare-name plane: `label` is the
    /// source id string (or `built-in`), `canonical` the claimant's canonical
    /// tool name.
    fn record_claim(
        claims: &mut BTreeMap<String, Vec<(String, String)>>,
        name: &str,
        label: &str,
        canonical: &str,
    ) {
        claims
            .entry(name.to_string())
            .or_default()
            .push((label.to_string(), canonical.to_string()));
    }

    /// Validate the candidate source names and adjudicate every bare-name
    /// contest into the mask table. See
    /// [`RuntimeCandidate::validate_candidate_source_names`] for the contract.
    fn adjudicate_source_name_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, String>) {
        const RESERVED_HEADS: [&str; 3] = ["mcp", "harness", "builtin"];
        const BUILTIN_LABEL: &str = "built-in";
        let valid_segment = |token: &str| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        };
        let claim_rank = |(label, _): &(String, String)| {
            let source_rank = u8::from(label != BUILTIN_LABEL);
            (source_rank, label.clone())
        };

        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Canonical name → owning source label, for hard duplicate detection
        // and for identifying the built-in remainder of the bare-name plane.
        let mut canonical_owner: BTreeMap<String, String> = BTreeMap::new();
        // Contested-name claims from contributed aliases: alias → claimants.
        let mut alias_claims: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                canonical_owner.insert(export.canonical_name.clone(), label.clone());
            }
        }

        for source in sources {
            let label = source.id.to_string();
            for export in &source.exports {
                let name = export.canonical_name.as_str();
                match source.id.kind {
                    RuntimeSourceKind::Plugin | RuntimeSourceKind::Bundle => {
                        let segments: Vec<&str> = name.split("__").collect();
                        let shaped = segments.len() >= 2
                            && segments.iter().all(|segment| valid_segment(segment))
                            && !RESERVED_HEADS.contains(&segments[0]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: tool name `{name}` must be qualified as \
                                 `namespace__local` with a non-reserved namespace token"
                            ));
                            continue;
                        }
                    }
                    RuntimeSourceKind::Mcp => {
                        let parts: Vec<&str> = name.split("__").collect();
                        let shaped = parts.len() == 3
                            && parts[0] == "mcp"
                            && valid_segment(parts[1])
                            && valid_segment(parts[2]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: MCP tool name `{name}` must be \
                                 `mcp__<server>__<local>` with valid tokens"
                            ));
                            continue;
                        }
                    }
                }
                if let Some(owner) = canonical_owner.get(name) {
                    conflicts.push(format!(
                        "source {label}: canonical name `{name}` is already provided by {owner}"
                    ));
                    continue;
                }
                canonical_owner.insert(name.to_string(), label.clone());
                for alias in &export.aliases {
                    if hya_tool::tool_bundle_presets()
                        .iter()
                        .any(|preset| preset.is_protected(alias))
                    {
                        conflicts.push(format!(
                            "source {label}: protected tool `{alias}` cannot be masked; \
                             the alias is rejected"
                        ));
                        continue;
                    }
                    if canonical_owner
                        .get(alias)
                        .is_some_and(|owner| owner == &label)
                    {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` collides with its own canonical name"
                        ));
                        continue;
                    }
                    let claimants = alias_claims.entry(alias.clone()).or_default();
                    if claimants.iter().any(|(owner, _)| owner == &label) {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` is declared more than once"
                        ));
                        continue;
                    }
                    claimants.push((label.clone(), export.canonical_name.clone()));
                }
            }
        }
        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                for alias in &export.aliases {
                    Self::record_claim(&mut alias_claims, alias, &label, &export.canonical_name);
                }
            }
        }

        // Adjudication: a name contested by more than one claimant resolves by
        // the masking total order — sources beat built-ins, and between two
        // sources the lexicographically greater source id string wins.
        // Canonical names owned by replaced sources still sit in the tool
        // snapshot during validation; they are about to be removed, so they
        // must not pose as built-in claims.
        let replaced_canonicals: BTreeSet<String> = self
            .sources
            .iter()
            .filter(|(id, _)| replaced.contains(&id.to_string()))
            .flat_map(|(_, source)| {
                source
                    .exports
                    .iter()
                    .map(|export| export.canonical_name.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let tool_canonicals: BTreeSet<String> = self
            .tools
            .snapshot()
            .canonical_tools()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut masks = BTreeMap::new();
        for (name, mut claimants) in alias_claims {
            if let Some(owner) = canonical_owner.get(&name) {
                claimants.push((owner.clone(), name.clone()));
            } else if !replaced_canonicals.contains(&name) && tool_canonicals.contains(&name) {
                claimants.push((BUILTIN_LABEL.to_string(), name.clone()));
            }
            if claimants.len() < 2 {
                continue;
            }
            claimants.sort_by_key(claim_rank);
            if let Some((_, winner)) = claimants.last() {
                masks.insert(name, winner.clone());
            }
        }
        (conflicts, masks)
    }

    /// Recompute the mask table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    fn recompute_masks_after_removal(&mut self) {
        let (_, masks) = self.adjudicate_source_name_claims(&[]);
        self.masks = masks;
    }

    /// Recompute the scheme table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    fn recompute_schemes_after_removal(&mut self) {
        let (_, schemes) = self.adjudicate_source_scheme_claims(&[]);
        self.schemes = schemes;
    }

    /// Validate every contributed source schema claim before any mutation
    /// happens, and derive the published scheme table:
    ///
    /// - the scheme must be a `[a-zA-Z0-9_-]` token of at least two characters
    ///   without the `__` separator (hard error);
    /// - the internal families `artifact`/`skill`/`local` are reserved and can
    ///   never be registered (hard error);
    /// - the claimed `canonical_tool` must be a canonical export of the *same*
    ///   source (hard error), so a source can only bind schemes to tools it
    ///   actually ships. `Bundle` sources are exempt: their tools are
    ///   view-scoped (sidecar activation) rather than registry exports, so a
    ///   bundle claims its owning tool by its `bundle:{id}/tool/{local}`
    ///   stable id instead;
    /// - a scheme claimed twice by one source is a declaration bug (hard
    ///   error);
    /// - a scheme claimed by two different sources resolves by the masking
    ///   total order: the lexicographically GREATER source id string wins,
    ///   approximating "the newer install wins". The table records only the
    ///   winner; the full claim chain stays derivable through
    ///   [`TurnBinding::scheme_chain`].
    ///
    /// All hard violations are reported in one grouped diagnostic.
    fn validate_candidate_source_schemes(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, SchemeBinding>, RuntimeRefreshError> {
        let (conflicts, schemes) = self.adjudicate_source_scheme_claims(sources);
        if conflicts.is_empty() {
            Ok(schemes)
        } else {
            Err(RuntimeRefreshError::SchemaConflicts(conflicts.join("\n")))
        }
    }

    /// Collect and adjudicate every scheme claim from the candidate sources
    /// and the retained (non-replaced) sources. See
    /// [`RuntimeCandidate::validate_candidate_source_schemes`] for the contract.
    fn adjudicate_source_scheme_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, SchemeBinding>) {
        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Scheme → claimants ordered by publication; the winner is the
        // lexicographically greatest source id (no built-in plane exists for
        // schemes: internal families are reserved outright).
        let mut claims: BTreeMap<String, Vec<(String, SchemeBinding)>> = BTreeMap::new();

        let record_claim =
            |source: &RuntimeSource,
             conflicts: &mut Vec<String>,
             claims: &mut BTreeMap<String, Vec<(String, SchemeBinding)>>| {
                let label = source.id.to_string();
                let mut declared: BTreeSet<&str> = BTreeSet::new();
                for schema in &source.schemas {
                    let scheme = schema.scheme.as_str();
                    if SchemeRegistry::is_internal_scheme(scheme) {
                        conflicts.push(format!(
                            "source {label}: internal scheme `{scheme}` is reserved and cannot \
                         be registered"
                        ));
                        continue;
                    }
                    if !SchemeRegistry::is_valid_scheme_token(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` must be a `[a-zA-Z0-9_-]` token \
                         of at least two characters without `__`"
                        ));
                        continue;
                    }
                    // Bundle sources keep their tools view-scoped (sidecar
                    // activation), so their schema claims name the owning
                    // tool by its `bundle:{id}/tool/{local}` stable id rather
                    // than a registry export.
                    if source.id.kind() != RuntimeSourceKind::Bundle
                        && source
                            .exports
                            .iter()
                            .all(|export| export.canonical_name != schema.canonical_tool)
                    {
                        conflicts.push(format!(
                            "source {label}: schema `{scheme}` claims tool `{}` which it does \
                         not export",
                            schema.canonical_tool
                        ));
                        continue;
                    }
                    if !declared.insert(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` is declared more than once"
                        ));
                        continue;
                    }
                    claims.entry(scheme.to_string()).or_default().push((
                        label.clone(),
                        SchemeBinding::new(
                            label.clone(),
                            schema.canonical_tool.clone(),
                            schema.writable,
                        ),
                    ));
                }
            };

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            record_claim(source, &mut conflicts, &mut claims);
        }
        for source in sources {
            record_claim(source, &mut conflicts, &mut claims);
        }

        // Adjudication: between two sources the lexicographically greater
        // source id string wins, mirroring the bare-name mask order.
        let schemes = claims
            .into_iter()
            .filter_map(|(scheme, mut claimants)| {
                claimants.sort_by(|left, right| left.0.cmp(&right.0));
                claimants.pop().map(|(_, binding)| (scheme, binding))
            })
            .collect();
        (conflicts, schemes)
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

fn runtime_source_dispatch_identity(
    source: &RuntimeSource,
    export: &RuntimeSourceExport,
) -> Result<[u8; 32], RuntimeRefreshError> {
    let mut bytes = Vec::new();
    append_identity_bytes(&mut bytes, RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1)?;
    append_identity_tag(&mut bytes, 1);
    append_identity_tag(
        &mut bytes,
        match source.id.kind {
            RuntimeSourceKind::Mcp => 0,
            RuntimeSourceKind::Plugin => 1,
            RuntimeSourceKind::Bundle => 2,
        },
    );
    append_identity_tag(&mut bytes, 2);
    append_identity_bytes(&mut bytes, source.id.configured_id.as_bytes())?;
    append_identity_tag(&mut bytes, 3);
    append_identity_bytes(&mut bytes, &source.declaration_digest)?;
    append_identity_tag(&mut bytes, 4);
    append_identity_count(&mut bytes, source.resources.len())?;
    for (key, value) in source.resources.iter() {
        append_identity_bytes(&mut bytes, key.as_bytes())?;
        append_canonical_json_value(&mut bytes, value)?;
    }
    append_identity_tag(&mut bytes, 5);
    append_identity_bytes(&mut bytes, export.declared_id.as_bytes())?;
    append_identity_tag(&mut bytes, 6);
    append_identity_bytes(&mut bytes, export.canonical_name.as_bytes())?;
    Ok(Sha256::digest(bytes).into())
}

pub(super) fn append_identity_tag(bytes: &mut Vec<u8>, tag: u8) {
    bytes.push(tag);
}

pub(super) fn append_identity_count(
    bytes: &mut Vec<u8>,
    count: usize,
) -> Result<(), RuntimeRefreshError> {
    let count = u64::try_from(count).map_err(|_| {
        RuntimeRefreshError::InvalidCandidate(
            "runtime source dispatch identity count exceeds u64".to_string(),
        )
    })?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

pub(super) fn append_model_map_identity(
    bytes: &mut Vec<u8>,
    models: &BTreeMap<String, ModelRef>,
) -> Result<(), RuntimeRefreshError> {
    append_identity_count(bytes, models.len())?;
    for (agent_id, model) in models {
        append_identity_bytes(bytes, agent_id.as_bytes())?;
        append_identity_bytes(bytes, model.as_str().as_bytes())?;
    }
    Ok(())
}

pub(super) fn append_model_configuration_identity(
    bytes: &mut Vec<u8>,
    configuration: &AgentModelConfiguration,
) -> Result<(), RuntimeRefreshError> {
    append_identity_tag(bytes, 1);
    append_model_map_identity(bytes, &configuration.builtin)?;
    append_identity_tag(bytes, 2);
    append_identity_count(bytes, configuration.bundles.len())?;
    for (bundle_id, models) in &configuration.bundles {
        append_identity_bytes(bytes, bundle_id.as_bytes())?;
        append_model_map_identity(bytes, models)?;
    }
    Ok(())
}

pub(super) fn append_identity_bytes(
    bytes: &mut Vec<u8>,
    value: &[u8],
) -> Result<(), RuntimeRefreshError> {
    let length = u64::try_from(value.len()).map_err(|_| {
        RuntimeRefreshError::InvalidCandidate(
            "runtime source dispatch identity length exceeds u64".to_string(),
        )
    })?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn append_canonical_json_value(
    bytes: &mut Vec<u8>,
    value: &Value,
) -> Result<(), RuntimeRefreshError> {
    match value {
        Value::Null => append_identity_tag(bytes, 0),
        Value::Bool(value) => {
            append_identity_tag(bytes, 1);
            append_identity_tag(bytes, u8::from(*value));
        }
        Value::Number(value) => {
            append_identity_tag(bytes, 2);
            let value = value.to_string();
            append_identity_bytes(bytes, value.as_bytes())?;
        }
        Value::String(value) => {
            append_identity_tag(bytes, 3);
            append_identity_bytes(bytes, value.as_bytes())?;
        }
        Value::Array(values) => {
            append_identity_tag(bytes, 4);
            append_identity_count(bytes, values.len())?;
            for value in values {
                append_canonical_json_value(bytes, value)?;
            }
        }
        Value::Object(values) => {
            append_identity_tag(bytes, 5);
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            append_identity_count(bytes, entries.len())?;
            for (key, value) in entries {
                append_identity_bytes(bytes, key.as_bytes())?;
                append_canonical_json_value(bytes, value)?;
            }
        }
    }
    Ok(())
}

pub(super) fn append_tool_view_identity(
    bytes: &mut Vec<u8>,
    tools: &ToolRegistrySnapshot,
) -> Option<()> {
    let mut canonical_tools = tools.canonical_tools();
    canonical_tools.sort_by(|left, right| left.0.cmp(&right.0));
    append_identity_count(bytes, canonical_tools.len()).ok()?;
    for (canonical_name, resolved) in canonical_tools {
        append_identity_tag(bytes, 1);
        append_identity_bytes(bytes, canonical_name.as_bytes()).ok()?;
        append_identity_tag(bytes, 2);
        append_tool_permission_identity(bytes, resolved.permission);

        let schema = resolved.tool.schema();
        append_identity_tag(bytes, 3);
        append_identity_bytes(bytes, schema.name.as_str().as_bytes()).ok()?;
        append_identity_tag(bytes, 4);
        append_identity_bytes(bytes, schema.description.as_bytes()).ok()?;
        append_identity_tag(bytes, 5);
        append_canonical_json_value(bytes, &schema.input_schema).ok()?;
        append_identity_tag(bytes, 6);
        match schema.output_schema {
            None => append_identity_tag(bytes, 0),
            Some(output_schema) => {
                append_identity_tag(bytes, 1);
                append_canonical_json_value(bytes, &output_schema).ok()?;
            }
        }

        append_identity_tag(bytes, 7);
        let dispatch_identity = tools.dispatch_identity_v1(&canonical_name)?;
        append_identity_bytes(bytes, &dispatch_identity).ok()?;

        append_identity_tag(bytes, 8);
        let mut aliases = tools.aliases_for_canonical(&canonical_name);
        aliases.sort();
        append_identity_count(bytes, aliases.len()).ok()?;
        for alias in aliases {
            append_identity_bytes(bytes, alias.as_bytes()).ok()?;
        }
    }
    Some(())
}

fn append_tool_permission_identity(bytes: &mut Vec<u8>, permission: ToolPermission) {
    append_identity_tag(
        bytes,
        match permission {
            ToolPermission::ReadOnly => 0,
            ToolPermission::Task => 1,
            ToolPermission::Tool => 2,
            ToolPermission::Command => 3,
            ToolPermission::Mcp => 4,
        },
    );
}

pub(super) fn append_skill_view_identity(
    bytes: &mut Vec<u8>,
    skills: &[SkillCatalogEntry],
) -> Option<()> {
    append_identity_count(bytes, skills.len()).ok()?;
    for skill in skills {
        append_identity_tag(bytes, 1);
        append_identity_bytes(bytes, skill.name.as_bytes()).ok()?;
        append_identity_tag(bytes, 2);
        append_identity_bytes(bytes, skill.description.as_bytes()).ok()?;
        append_identity_tag(bytes, 3);
        let content_digest = Sha256::digest(skill.content.as_bytes());
        append_identity_bytes(bytes, &content_digest).ok()?;
        append_identity_tag(bytes, 4);
        let mut allowed_tools = skill.allowed_tools.clone();
        allowed_tools.sort();
        append_identity_count(bytes, allowed_tools.len()).ok()?;
        for allowed_tool in allowed_tools {
            append_identity_bytes(bytes, allowed_tool.as_bytes()).ok()?;
        }
        append_identity_tag(bytes, 5);
        match skill.model.as_deref() {
            None => append_identity_tag(bytes, 0),
            Some(model) => {
                append_identity_tag(bytes, 1);
                append_identity_bytes(bytes, model.as_bytes()).ok()?;
            }
        }
        append_identity_tag(bytes, 6);
        append_identity_bytes(bytes, skill.path.to_str()?.as_bytes()).ok()?;
        append_identity_tag(bytes, 7);
        append_identity_bytes(bytes, skill.dir.to_str()?.as_bytes()).ok()?;
    }
    Some(())
}

pub(super) fn append_runtime_source_view_identity(
    bytes: &mut Vec<u8>,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    tools: &ToolRegistrySnapshot,
) -> Option<()> {
    append_identity_count(bytes, sources.len()).ok()?;
    for (source_id, source) in sources {
        append_identity_tag(bytes, 1);
        append_identity_tag(
            bytes,
            match source_id.kind {
                RuntimeSourceKind::Mcp => 0,
                RuntimeSourceKind::Plugin => 1,
                RuntimeSourceKind::Bundle => 2,
            },
        );
        append_identity_tag(bytes, 2);
        append_identity_bytes(bytes, source_id.configured_id.as_bytes()).ok()?;
        append_identity_tag(bytes, 3);
        append_identity_bytes(bytes, &source.declaration_digest).ok()?;
        append_identity_tag(bytes, 4);
        append_identity_count(bytes, source.resources.len()).ok()?;
        for (key, value) in source.resources.iter() {
            append_identity_bytes(bytes, key.as_bytes()).ok()?;
            append_canonical_json_value(bytes, value).ok()?;
        }

        append_identity_tag(bytes, 5);
        append_identity_count(bytes, source.skills.len()).ok()?;
        for skill in &source.skills {
            append_identity_tag(bytes, 1);
            append_identity_bytes(bytes, skill.stable_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 2);
            append_identity_bytes(bytes, skill.local_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 3);
            let mut aliases = skill.aliases.clone();
            aliases.sort();
            append_identity_count(bytes, aliases.len()).ok()?;
            for alias in aliases {
                append_identity_bytes(bytes, alias.as_bytes()).ok()?;
            }
            append_identity_tag(bytes, 4);
            append_identity_bytes(bytes, skill.digest.as_bytes()).ok()?;
            append_identity_tag(bytes, 5);
            append_identity_bytes(bytes, skill.content.as_bytes()).ok()?;
            append_identity_tag(bytes, 6);
            append_skill_view_identity(bytes, std::slice::from_ref(&skill.entry))?;
        }

        append_identity_tag(bytes, 6);
        append_identity_count(bytes, source.exports.len()).ok()?;
        for export in &source.exports {
            append_identity_tag(bytes, 1);
            append_identity_bytes(bytes, export.declared_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 2);
            append_identity_bytes(bytes, export.canonical_name.as_bytes()).ok()?;
            append_identity_tag(bytes, 3);
            append_tool_permission_identity(bytes, export.permission);
            append_identity_tag(bytes, 4);
            let mut aliases = export.aliases.clone();
            aliases.sort();
            append_identity_count(bytes, aliases.len()).ok()?;
            for alias in aliases {
                append_identity_bytes(bytes, alias.as_bytes()).ok()?;
            }
            append_identity_tag(bytes, 5);
            let expected = runtime_source_dispatch_identity(source, export).ok()?;
            let actual = tools.dispatch_identity_v1(&export.canonical_name)?;
            if actual != expected {
                return None;
            }
            append_identity_bytes(bytes, &expected).ok()?;
        }
    }
    Some(())
}
