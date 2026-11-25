use super::*;

const RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.core.runtime-source-dispatch/v1";
const RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2: &[u8] = b"hya.core.runtime-semantic-fingerprint/v2";

impl TurnBinding {
    /// Return a deterministic identity for the currently supported complete
    /// runtime view. Views with unidentifiable sources are intentionally
    /// unavailable until their semantic sections have a canonical encoding.
    #[must_use]
    /// Domain-separated fingerprint of agents, tools, sources, and permissions.
    ///
    /// v2 folds in the compiled-in built-in roster digest, which replaced the
    /// prepared-catalog digests the retired built-in bundles contributed.
    pub fn semantic_fingerprint_v1(&self, permission: &PermissionPlane) -> Option<[u8; 32]> {
        let catalog_identity = self.snapshot.catalog.semantic_identity_v1()?;
        let permission_identity = permission.semantic_identity_v1()?;
        let mut bytes = Vec::new();
        append_identity_bytes(&mut bytes, RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2).ok()?;

        append_identity_tag(&mut bytes, 1);
        append_identity_bytes(&mut bytes, &catalog_identity).ok()?;

        // The explicit empty section represents the none effective view.
        append_identity_tag(&mut bytes, 2);
        append_identity_count(&mut bytes, 0).ok()?;

        append_identity_tag(&mut bytes, 3);
        append_tool_view_identity(&mut bytes, &self.snapshot.basic_tools)?;

        append_identity_tag(&mut bytes, 4);
        append_tool_view_identity(&mut bytes, &self.snapshot.tools)?;

        append_identity_tag(&mut bytes, 5);
        let skills = self
            .snapshot
            .skills
            .get(&self.place.workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        append_skill_view_identity(&mut bytes, skills)?;

        append_identity_tag(&mut bytes, 6);
        append_runtime_source_view_identity(
            &mut bytes,
            &self.snapshot.sources,
            &self.snapshot.tools,
        )?;

        append_identity_tag(&mut bytes, 7);
        append_identity_bytes(&mut bytes, &permission_identity).ok()?;

        if !self.agent_model_configuration.builtin.is_empty()
            || self
                .agent_model_configuration
                .bundles
                .values()
                .any(|models| !models.is_empty())
        {
            append_identity_tag(&mut bytes, 8);
            append_model_configuration_identity(&mut bytes, &self.agent_model_configuration)
                .ok()?;
        }
        if !self.session_agent_models.is_empty() {
            append_identity_tag(&mut bytes, 9);
            append_model_map_identity(&mut bytes, &self.session_agent_models).ok()?;
        }

        Some(Sha256::digest(bytes).into())
    }
}

pub(super) fn runtime_source_dispatch_identity(
    source: &RuntimeSource,
    export: &RuntimeSourceExport,
) -> Result<[u8; 32], RuntimeRefreshError> {
    let mut bytes = Vec::new();
    append_identity_bytes(&mut bytes, RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1)?;
    append_identity_tag(&mut bytes, 1);
    append_identity_tag(
        &mut bytes,
        match source.id.kind() {
            RuntimeSourceKind::Mcp => 0,
            RuntimeSourceKind::Plugin => 1,
            RuntimeSourceKind::Bundle => 2,
        },
    );
    append_identity_tag(&mut bytes, 2);
    append_identity_bytes(&mut bytes, source.id.configured_id().as_bytes())?;
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

fn append_identity_tag(bytes: &mut Vec<u8>, tag: u8) {
    bytes.push(tag);
}

fn append_identity_count(bytes: &mut Vec<u8>, count: usize) -> Result<(), RuntimeRefreshError> {
    let count = u64::try_from(count).map_err(|_| {
        RuntimeRefreshError::InvalidCandidate(
            "runtime source dispatch identity count exceeds u64".to_string(),
        )
    })?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_model_map_identity(
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

fn append_model_configuration_identity(
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

fn append_identity_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), RuntimeRefreshError> {
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

fn append_tool_view_identity(bytes: &mut Vec<u8>, tools: &ToolRegistrySnapshot) -> Option<()> {
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

fn append_skill_view_identity(bytes: &mut Vec<u8>, skills: &[SkillCatalogEntry]) -> Option<()> {
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

fn append_runtime_source_view_identity(
    bytes: &mut Vec<u8>,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    tools: &ToolRegistrySnapshot,
) -> Option<()> {
    append_identity_count(bytes, sources.len()).ok()?;
    for (source_id, source) in sources {
        append_identity_tag(bytes, 1);
        append_identity_tag(
            bytes,
            match source_id.kind() {
                RuntimeSourceKind::Mcp => 0,
                RuntimeSourceKind::Plugin => 1,
                RuntimeSourceKind::Bundle => 2,
            },
        );
        append_identity_tag(bytes, 2);
        append_identity_bytes(bytes, source_id.configured_id().as_bytes()).ok()?;
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
