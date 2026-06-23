use std::collections::BTreeMap;
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

mod vcs;

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
    description: &'static str,
    mode: &'static str,
    native: bool,
    permission: Vec<PermissionRule>,
    model: AgentModel,
    prompt: String,
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
        state: env_path("XDG_STATE_HOME", &home, ".local/state/yaca"),
        config: env_path("XDG_CONFIG_HOME", &home, ".config/yaca"),
        worktree: workdir.to_string_lossy().into_owned(),
        directory: workdir.to_string_lossy().into_owned(),
    })
}

async fn agent(
    axum::extract::State(st): axum::extract::State<ServerState>,
) -> Json<Vec<AgentInfo>> {
    Json(vec![AgentInfo {
        name: st.agent.name.to_string(),
        description: "The default agent. Executes tools based on configured permissions.",
        mode: "primary",
        native: true,
        permission: super::agent_permission::from_engine(&st.engine),
        model: model_info(st.agent.model.as_str()),
        prompt: st.agent.system_prompt.clone(),
        options: json!({}),
    }])
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
        provider_id: "yaca".to_string(),
    }
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
