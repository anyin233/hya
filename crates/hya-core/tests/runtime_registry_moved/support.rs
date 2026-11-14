use super::*;
/// Test shim: wrap prepared AgentBundles as an [`AgentCatalog`] over the
/// compiled-in built-in roster, so unit tests keep their existing shape.
pub(super) struct TestCatalog;

impl TestCatalog {
    pub(super) fn from_prepared(bundles: &[Bundle]) -> Result<AgentCatalog, BundleError> {
        AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(bundles)?))
    }
    pub(super) fn from_verified_catalogs(
        catalogs: &[&hya_bundle::PreparedCatalog],
    ) -> Result<AgentCatalog, BundleError> {
        AgentCatalog::new(Arc::new(BundleCatalog::from_verified_catalogs(catalogs)?))
    }
}

/// Build a test registry and publish every prepared bundle Skill through the runtime source seam.
pub(super) fn test_runtime_registry(
    tools: ToolRegistry,
    catalog: Arc<AgentCatalog>,
) -> RuntimeRegistry {
    let sources = catalog
        .bundles()
        .bundles()
        .iter()
        .filter_map(|bundle| {
            let skills = bundle
                .resources(hya_bundle::ExportKind::Skill)
                .iter()
                .map(|resource| {
                    let parsed = hya_tool::parse_skill(&resource.content)
                        .expect("test bundle Skill must contain valid frontmatter");
                    let path = PathBuf::from(format!(
                        "bundle:{}/{}",
                        bundle.identity().id,
                        resource.source_path
                    ));
                    let dir = path
                        .parent()
                        .expect("test bundle Skill path must have a parent")
                        .to_path_buf();
                    RuntimeSourceSkill::new(
                        resource.stable_id.clone(),
                        resource.local_id.clone(),
                        resource.aliases.clone(),
                        resource.digest.clone(),
                        resource.content.clone(),
                        SkillCatalogEntry {
                            name: parsed.name,
                            description: parsed.description,
                            content: parsed.content,
                            allowed_tools: parsed.allowed_tools,
                            model: parsed.model,
                            path,
                            dir,
                            origin: hya_tool::SkillCatalogOrigin::Virtual,
                        },
                    )
                })
                .collect::<Vec<_>>();
            (!skills.is_empty()).then(|| {
                RuntimeSource::new(
                    RuntimeSourceId::bundle(bundle.identity().id.clone()),
                    [0; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_skills(skills)
            })
        })
        .collect::<Vec<_>>();
    let registry = RuntimeRegistry::new(tools, catalog);
    if !sources.is_empty() {
        registry
            .refresh(|candidate| {
                candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, sources)
            })
            .expect("test bundle Skill sources must publish");
    }
    registry
}

pub(super) struct NoopTool {
    name: String,
}

impl NoopTool {
    pub(super) fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "noop".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

pub(super) fn bundle_with_agent(
    bundle_id: &str,
    agent: PreparedAgent,
    skills: Vec<PreparedResource>,
) -> Bundle {
    Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: bundle_id.to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        namespace: None,
        digest: "test-only".to_string(),
        agents: vec![agent],
        tools: Vec::new(),
        skills,
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        tui: None,

        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        permission_modes: vec![],
    }
}

pub(super) fn agent(stable_id: &str, resource_view: ResourceView) -> PreparedAgent {
    PreparedAgent {
        id: AgentName::new(stable_id),
        description: None,
        role: AgentRole::Main,
        color: None,
        prompt: Some("prompt".to_string()),
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        resource_view,
        can_spawn: Vec::new(),
        hook_refs: Vec::new(),
        advertise: Vec::new(),
        handoff: hya_bundle::HandoffPolicy::Model,
    }
}

pub(super) fn scheme_source(
    id: &str,
    exports: Vec<RuntimeSourceExport>,
    schemas: Vec<SourceSchema>,
) -> RuntimeSource {
    RuntimeSource::new(RuntimeSourceId::plugin(id), [0; 32], Arc::new(()), exports)
        .with_schemas(schemas)
}

pub(super) fn bundle_scheme_source(
    bundle_id: &str,
    scheme: &str,
    canonical_tool: &str,
) -> RuntimeSource {
    RuntimeSource::new(
        RuntimeSourceId::bundle(bundle_id),
        [7; 32],
        Arc::new(()),
        Vec::new(),
    )
    .with_schemas(vec![SourceSchema {
        scheme: scheme.to_string(),
        canonical_tool: canonical_tool.to_string(),
        writable: false,
    }])
}
