//! `/v1` catalog domain: agents, models, providers, commands, skills,
//! tools, permission modes, and saved permission rules.

use std::collections::BTreeMap;
use std::path::Path;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;
use serde_json::{Value, json};

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/agents", get(list_agents))
        .route("/v1/models", get(list_models))
        .route("/v1/providers", get(list_providers))
        .route(
            "/v1/providers/:provider_id",
            get(get_provider).put(super::providers::upsert_provider),
        )
        .route(
            "/v1/providers/:provider_id/refresh",
            post(super::providers::refresh_provider),
        )
        .route(
            "/v1/providers/:provider_id/models",
            put(super::providers::set_provider_model)
                .delete(super::providers::remove_provider_model),
        )
        .route(
            "/v1/providers/:provider_id/test",
            post(super::providers::test_provider_model),
        )
        .route("/v1/commands", get(list_commands))
        .route("/v1/skills", get(list_skills))
        .route("/v1/tools", get(list_tools))
        .route("/v1/permission-modes", get(list_permission_modes))
        .route("/v1/runtime/schemas", get(list_runtime_schemas))
        .route("/v1/permissions/rules", get(list_saved_rules))
        .route("/v1/permissions/rules/:rule", delete(delete_saved_rule))
}

async fn list_agents(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListAgentsResponse>, V1Error> {
    let request: pb::ListAgentsRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &request.directory);
    let agents = agent_rows(&st, &workdir).await?;
    Ok(Json(paginated_agents(agents, &request.page)))
}

/// Agent rows shared with the bootstrap snapshot.
pub(crate) async fn agent_rows(
    st: &ServerState,
    workdir: &Path,
) -> Result<Vec<pb::AgentSummary>, V1Error> {
    let rows = crate::support::bound_agent_metadata::list(st, workdir)
        .await
        .map_err(|error| V1Error::new(hya_api::error::Code::Internal, error.text().to_owned()))?;
    Ok(rows
        .into_iter()
        .map(|row| pb::AgentSummary {
            name: row.name.clone(),
            model: row.model.as_deref().and_then(model_ref),
            description: row.description.clone().unwrap_or_default(),
            hidden: row.hidden,
        })
        .collect())
}

/// Parse a `provider/model[#variant]` reference string.
pub(crate) fn model_ref(reference: &str) -> Option<pb::ModelRef> {
    let (provider, rest) = reference.split_once('/')?;
    let (model, variant) = match rest.split_once('#') {
        Some((model, variant)) => (model, Some(variant.to_owned())),
        None => (rest, None),
    };
    Some(pb::ModelRef {
        provider_id: provider.to_owned(),
        model_id: model.to_owned(),
        variant: variant.unwrap_or_default(),
    })
}

fn paginated_agents(
    agents: Vec<pb::AgentSummary>,
    page: &Option<pb::PageRequest>,
) -> pb::ListAgentsResponse {
    let (agents, info) = paginate(agents, page);
    pb::ListAgentsResponse {
        agents,
        page: Some(info),
    }
}

/// Apply cursor pagination over a fully materialized page.
pub(crate) fn paginate<T>(rows: Vec<T>, page: &Option<pb::PageRequest>) -> (Vec<T>, pb::PageInfo) {
    let offset = page
        .as_ref()
        .and_then(|page| hya_api::cursor::decode_offset(&page.cursor).ok())
        .unwrap_or(0);
    let limit = page.as_ref().map_or(50, |page| {
        if page.limit == 0 {
            50
        } else {
            page.limit.min(500) as usize
        }
    });
    let total = rows.len();
    let rows = rows
        .into_iter()
        .skip((offset as usize).min(total))
        .take(limit)
        .collect::<Vec<_>>();
    let taken = rows.len();
    let next_offset = offset.saturating_add(taken as u64);
    let has_more = (next_offset as usize) < total;
    let info = pb::PageInfo {
        next_cursor: if has_more {
            hya_api::cursor::encode_offset(next_offset)
        } else {
            String::new()
        },
        has_more,
    };
    (rows, info)
}

async fn list_models(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListModelsResponse>, V1Error> {
    let request: pb::ListModelsRequest = super::query_request(&[], &query)?;
    let _scope = scope_directory(&headers, &request.directory);
    let mut models = model_rows(&st);
    if !request.provider_id.is_empty() {
        models.retain(|model| model.provider_id == request.provider_id);
    }
    let (models, page) = paginate(models, &request.page);
    Ok(Json(pb::ListModelsResponse {
        models,
        page: Some(page),
    }))
}

/// Model rows shared with the bootstrap snapshot.
pub(crate) fn model_rows(st: &ServerState) -> Vec<pb::ModelSummary> {
    let snapshot = st.engine.provider_catalog_snapshot();
    let auth_by_provider: std::collections::BTreeMap<String, i32> = snapshot
        .providers()
        .iter()
        .map(|state| (state.provider_id.clone(), auth_status(state.auth)))
        .collect();
    snapshot
        .models()
        .iter()
        .map(|row| pb::ModelSummary {
            id: format!("{}/{}", row.provider_id, row.model_id),
            provider_id: row.provider_id.clone(),
            model_id: row.model_id.clone(),
            display_name: row.display_name.clone().unwrap_or_default(),
            reasoning: !row.reasoning_variants.is_empty(),
            auth: *auth_by_provider
                .get(&row.provider_id)
                .unwrap_or(&(pb::AuthStatus::NotApplicable as i32)),
            context_limit: u64::from(row.capabilities.max_context),
            output_limit: u64::from(row.capabilities.max_output),
            source: row.source.as_str().to_owned(),
        })
        .collect()
}

/// Map the provider auth state onto the wire enum.
fn auth_status(auth: hya_provider::ProviderAuthState) -> i32 {
    use hya_provider::ProviderAuthState as A;
    match auth {
        A::Credentialed => pb::AuthStatus::Credentialed as i32,
        A::Unauthenticated => pb::AuthStatus::Unauthenticated as i32,
        A::AuthRequired => pb::AuthStatus::AuthRequired as i32,
        A::AuthRejected => pb::AuthStatus::AuthRejected as i32,
        A::NotApplicable => pb::AuthStatus::NotApplicable as i32,
    }
}

/// Map the provider discovery outcome onto its wire string.
fn catalog_result(result: hya_provider::ProviderCatalogResult) -> String {
    use hya_provider::ProviderCatalogResult as R;
    match result {
        R::Models => "models",
        R::Empty => "empty",
        R::Unavailable => "unavailable",
        R::Invalid => "invalid",
        R::Unsupported | R::Offline => "unavailable",
    }
    .to_owned()
}

/// Config `kind` label for a live route kind (used when the provider is not
/// in the settings list, e.g. an embedder without a provider control).
fn kind_label(kind: hya_provider::ProviderKind) -> &'static str {
    use hya_provider::ProviderKind as K;
    match kind {
        K::OpenAiCompatible => "openai",
        K::OpenAiResponse => "openai-response",
        K::OpenAiCodex => "openai-codex",
        K::GrokBuild => "grok-build",
        K::Anthropic => "anthropic",
        K::Google => "google",
    }
}

async fn list_providers(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListProvidersResponse>, V1Error> {
    let request: pb::ListProvidersRequest = super::query_request(&[], &query)?;
    let _scope = scope_directory(&headers, &request.directory);
    let providers = provider_rows(&st, &model_rows(&st)).await;
    let (providers, page) = paginate(providers, &request.page);
    Ok(Json(pb::ListProvidersResponse {
        providers,
        page: Some(page),
    }))
}

/// Provider rows shared with the bootstrap snapshot: every provider in the
/// live catalog plus any configured provider the catalog does not hold yet,
/// sorted by id. `auth` reflects the current credential (saved key, config
/// key, or none) unless the last model-list fetch was refused.
pub(crate) async fn provider_rows(
    st: &ServerState,
    models: &[pb::ModelSummary],
) -> Vec<pb::ProviderSummary> {
    let settings = st
        .provider_control
        .list_settings()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|settings| (settings.id.clone(), settings))
        .collect::<BTreeMap<_, _>>();
    let snapshot = st.engine.provider_catalog_snapshot();
    let mut rows = BTreeMap::new();
    for state in snapshot.providers() {
        let settings = settings.get(&state.provider_id);
        let offline = state.source == hya_provider::ProviderCatalogSource::Offline;
        let auth = match (state.auth, settings.map(|settings| settings.key_source)) {
            (
                hya_provider::ProviderAuthState::AuthRejected
                | hya_provider::ProviderAuthState::AuthRequired
                | hya_provider::ProviderAuthState::NotApplicable,
                _,
            )
            | (_, None) => auth_status(state.auth),
            (_, Some(crate::ProviderKeySource::None)) => pb::AuthStatus::Unauthenticated as i32,
            (_, Some(_)) => pb::AuthStatus::Credentialed as i32,
        };
        rows.insert(
            state.provider_id.clone(),
            pb::ProviderSummary {
                id: state.provider_id.clone(),
                name: state.provider_id.clone(),
                auth,
                website: String::new(),
                result: catalog_result(state.result),
                kind: settings.map_or_else(
                    || {
                        if offline {
                            String::new()
                        } else {
                            kind_label(state.kind).to_owned()
                        }
                    },
                    |settings| settings.kind.clone(),
                ),
                base_url: settings
                    .map(|settings| settings.base_url.clone())
                    .unwrap_or_default(),
                key_source: settings
                    .map_or(crate::ProviderKeySource::None, |settings| {
                        settings.key_source
                    })
                    .as_str()
                    .to_owned(),
                model_count: 0,
            },
        );
    }
    for (id, settings) in &settings {
        rows.entry(id.clone())
            .or_insert_with(|| pb::ProviderSummary {
                id: id.clone(),
                name: id.clone(),
                auth: if settings.key_source == crate::ProviderKeySource::None {
                    pb::AuthStatus::Unauthenticated as i32
                } else {
                    pb::AuthStatus::Credentialed as i32
                },
                website: String::new(),
                result: "unavailable".to_owned(),
                kind: settings.kind.clone(),
                base_url: settings.base_url.clone(),
                key_source: settings.key_source.as_str().to_owned(),
                model_count: 0,
            });
    }
    for model in models {
        if let Some(row) = rows.get_mut(&model.provider_id) {
            row.model_count = row.model_count.saturating_add(1);
        }
    }
    rows.into_values().collect()
}

/// One provider's detail row with its model rows; `None` when the id is
/// neither live nor configured.
pub(crate) async fn provider_info(st: &ServerState, provider_id: &str) -> Option<pb::ProviderInfo> {
    let all_models = model_rows(st);
    let summary = provider_rows(st, &all_models)
        .await
        .into_iter()
        .find(|row| row.id == provider_id)?;
    let models = all_models
        .into_iter()
        .filter(|model| model.provider_id == provider_id)
        .collect();
    let supports_oauth = matches!(summary.kind.as_str(), "openai-codex" | "grok-build");
    Some(pb::ProviderInfo {
        summary: Some(summary),
        models,
        supports_api_key: true,
        supports_oauth,
    })
}

async fn get_provider(
    State(st): State<ServerState>,
    AxumPath(provider_id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ProviderInfo>, V1Error> {
    let request: pb::GetProviderRequest =
        super::query_request(&[("provider_id", provider_id.as_str())], &query)?;
    let _scope = scope_directory(&headers, &request.directory);
    provider_info(&st, &request.provider_id)
        .await
        .map(Json)
        .ok_or_else(|| {
            V1Error::new(
                hya_api::error::Code::NotFound,
                format!("provider not found: {}", request.provider_id),
            )
        })
}

async fn list_commands(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListCommandsResponse>, V1Error> {
    let request: pb::ListCommandsRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &request.directory);
    let commands = command_rows(&workdir);
    let (commands, page) = paginate(commands, &request.page);
    Ok(Json(pb::ListCommandsResponse {
        commands,
        page: Some(page),
    }))
}

/// Command rows shared with the bootstrap snapshot.
pub(crate) fn command_rows(workdir: &Path) -> Vec<pb::CommandSummary> {
    crate::support::command_catalog::list(workdir)
        .into_iter()
        .map(|row| pb::CommandSummary {
            name: row.name.clone(),
            description: row.description.clone().unwrap_or_default(),
            argument_hint: String::new(),
            hints: row.hints.clone(),
            source: row.source.to_owned(),
            template: row.template.clone(),
            agent: row.agent.clone().unwrap_or_default(),
            model: row.model.clone().unwrap_or_default(),
            subtask: row.subtask,
        })
        .collect()
}

async fn list_skills(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListSkillsResponse>, V1Error> {
    let request: pb::ListSkillsRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &request.directory);
    let skills = skill_rows(&workdir);
    let (skills, page) = paginate(skills, &request.page);
    Ok(Json(pb::ListSkillsResponse {
        skills,
        page: Some(page),
    }))
}

/// Skill rows shared with the bootstrap snapshot.
pub(crate) fn skill_rows(workdir: &Path) -> Vec<pb::SkillSummary> {
    crate::support::skill_catalog::list(workdir)
        .into_iter()
        .map(|row| pb::SkillSummary {
            name: row.name.clone(),
            description: row.description.clone(),
            source: "builtin".to_owned(),
            content: row.content.clone(),
            location: row.location.clone(),
        })
        .collect()
}

async fn list_tools(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListToolsResponse>, V1Error> {
    let request: pb::ListToolsRequest = super::query_request(&[], &query)?;
    let _scope = scope_directory(&headers, &request.directory);
    let tools = tool_rows(&st);
    let (tools, page) = paginate(tools, &request.page);
    Ok(Json(pb::ListToolsResponse {
        tools,
        page: Some(page),
    }))
}

async fn list_permission_modes(
    State(st): State<ServerState>,
) -> Result<Json<pb::ListPermissionModesResponse>, V1Error> {
    let modes = st
        .engine
        .permission_modes()
        .await
        .into_iter()
        .map(|mode| pb::PermissionModeSummary {
            id: mode.id,
            title: mode.title,
            description: mode.description,
            source: mode.source,
        })
        .collect();
    Ok(Json(pb::ListPermissionModesResponse { modes }))
}

/// Tool rows shared with the bootstrap snapshot.
pub(crate) fn tool_rows(st: &ServerState) -> Vec<pb::ToolSummary> {
    st.engine
        .tool_schemas()
        .into_iter()
        .map(|schema| pb::ToolSummary {
            name: schema.name.to_string(),
            description: schema.description.clone(),
            kind: "builtin".to_owned(),
            hidden: false,
        })
        .collect()
}

/// The runtime snapshot's published scheme table: registered external URI
/// schemes with their winning source binding and masking chain.
///
/// Rendered as plain protojson-shaped data until the `hya.v1` IDL grows the
/// matching message; the route exists so clients can already observe the table.
async fn list_runtime_schemas(State(st): State<ServerState>) -> Json<Value> {
    let registry = st.engine.runtime_registry();
    let effective = registry.effective_schemes();
    let schemas = effective
        .schemes
        .keys()
        .map(|scheme| {
            let binding = &effective.schemes[scheme];
            json!({
                "scheme": scheme,
                "owner": binding.owner(),
                "canonicalTool": binding.canonical_tool(),
                "writable": binding.writable(),
                "chain": registry
                    .scheme_chain(scheme)
                    .into_iter()
                    .map(|(owner, canonical_tool)| json!({
                        "owner": owner,
                        "canonicalTool": canonical_tool,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "generation": effective.generation.get(),
        "schemas": schemas,
    }))
}

async fn list_saved_rules(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListSavedRulesResponse>, V1Error> {
    let request: pb::ListSavedRulesRequest = super::query_request(&[], &query)?;
    let _scope = scope_directory(&headers, &request.directory);
    let rules = saved_rule_rows(&st).await;
    let (rules, page) = paginate(rules, &request.page);
    Ok(Json(pb::ListSavedRulesResponse {
        rules,
        page: Some(page),
    }))
}

/// Saved permission rules shared with the bootstrap snapshot.
pub(crate) async fn saved_rule_rows(st: &ServerState) -> Vec<pb::SavedRule> {
    let Ok(rows) = st.permission_requests.list_saved(None).await else {
        return Vec::new();
    };
    rows.into_iter()
        .map(|row| pb::SavedRule {
            id: field(&row, "id"),
            permission: pb::RulePermission::Ask as i32,
            tool: field(&row, "action"),
            pattern: field(&row, "resource"),
            time_created: None,
        })
        .collect()
}

/// Extract a serialized field from a Compat row type without widening its
/// private field visibility.
fn field<T: serde::Serialize>(row: &T, name: &str) -> String {
    serde_json::to_value(row)
        .ok()
        .and_then(|value| value.get(name).cloned())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn delete_saved_rule(
    State(st): State<ServerState>,
    AxumPath(rule): AxumPath<String>,
) -> Result<Json<pb::DeleteSavedRuleResponse>, V1Error> {
    st.permission_requests
        .remove_saved(&rule)
        .await
        .map_err(V1Error::from)?;
    Ok(Json(pb::DeleteSavedRuleResponse {}))
}
