use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ServerState;

use super::agent_permission::PermissionRule;

pub(in crate::compat) mod vcs;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(vcs::router())
        .route("/instance/dispose", post(dispose))
        .route("/path", get(path))
        .route("/command", get(command))
        .route("/agent", get(agent))
        .route("/skill", get(skill))
        .route("/lsp", get(lsp))
}

#[derive(Serialize)]
struct PathInfo {
    home: String,
    state: String,
    config: String,
    worktree: String,
    directory: String,
}

#[derive(Serialize)]
struct AgentInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    mode: String,
    native: bool,
    #[serde(skip_serializing_if = "is_false")]
    hidden: bool,
    permission: Vec<PermissionRule>,
    model: AgentModel,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    steps: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    options: Value,
}

#[derive(Serialize)]
struct AgentModel {
    #[serde(rename = "modelID")]
    model_id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
}

async fn dispose() -> Json<bool> {
    Json(true)
}

async fn path(
    axum::extract::State(st): axum::extract::State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<PathInfo> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    let home = home_dir();
    Json(PathInfo {
        home: home.to_string_lossy().into_owned(),
        state: env_path("XDG_STATE_HOME", &home, ".local/state/hya"),
        config: env_path("XDG_CONFIG_HOME", &home, ".config/hya"),
        worktree: workdir.to_string_lossy().into_owned(),
        directory: workdir.to_string_lossy().into_owned(),
    })
}

async fn agent(
    axum::extract::State(st): axum::extract::State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<Vec<AgentInfo>> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    let build_permissions = super::agent_permission::from_engine(&st.engine);
    Json(
        super::agent_catalog::list(&workdir, &st)
            .into_iter()
            .map(|agent| AgentInfo {
                name: agent.name.clone(),
                description: agent.description,
                mode: agent.mode,
                native: agent.native,
                hidden: agent.hidden,
                permission: agent_permissions(&agent.name, &build_permissions, agent.permissions),
                model: model_info(agent.model.as_deref().unwrap_or(st.agent.model.as_str())),
                temperature: agent.temperature,
                top_p: agent.top_p,
                color: agent.color,
                steps: agent.steps,
                prompt: if agent.name == "build" && agent.prompt.is_none() {
                    Some(st.agent.system_prompt.clone())
                } else {
                    agent.prompt
                },
                options: json!(agent.options),
            })
            .collect(),
    )
}

async fn command(
    axum::extract::State(st): axum::extract::State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<Vec<super::command_catalog::CommandInfo>> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(super::command_catalog::list(&workdir))
}

async fn skill(
    axum::extract::State(st): axum::extract::State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<Vec<super::skill_catalog::SkillInfo>> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(super::skill_catalog::list(&workdir))
}

async fn lsp(
    axum::extract::State(st): axum::extract::State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<Vec<Value>> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(st.engine.lsp().status(&workdir).await.unwrap_or_default())
}

fn model_info(model: &str) -> AgentModel {
    if let Some((provider, model_id)) = model.split_once('/') {
        return AgentModel {
            model_id: model_id.to_string(),
            provider_id: provider.to_string(),
        };
    }
    AgentModel {
        model_id: model.to_string(),
        provider_id: "hya".to_string(),
    }
}

pub(super) fn agent_permissions(
    name: &str,
    build_permissions: &[PermissionRule],
    configured: Vec<PermissionRule>,
) -> Vec<PermissionRule> {
    let mut permissions = if name == "build" {
        build_permissions.to_vec()
    } else {
        Vec::new()
    };
    permissions.extend(configured);
    permissions
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_path(var: &str, home: &Path, suffix: &str) -> String {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(suffix))
        .to_string_lossy()
        .into_owned()
}
