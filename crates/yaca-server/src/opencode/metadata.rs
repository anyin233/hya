use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ServerState;

use super::agent_permission::PermissionRule;
use super::location::{LocationInfo, LocationRef, LocationResponse};
use super::model_ref::model_ref_parts;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/location", get(location))
        .route("/api/agent", get(agent))
        .route("/api/command", get(command))
        .route("/api/skill", get(skill))
}

#[derive(Serialize)]
struct AgentInfo {
    id: String,
    model: AgentModelRef,
    request: RequestInfo,
    system: String,
    mode: &'static str,
    hidden: bool,
    permissions: Vec<PermissionRule>,
}

#[derive(Serialize)]
struct AgentModelRef {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
}

#[derive(Serialize)]
struct RequestInfo {
    headers: BTreeMap<String, String>,
    body: Value,
}

#[derive(Serialize)]
struct SkillInfo {
    name: String,
    description: String,
    location: String,
    content: String,
}

async fn location(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationInfo> {
    let location = LocationRef::from_request(&query, &headers);
    Json(super::location::info_at(&st, &location))
}

async fn agent(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<AgentInfo>>> {
    let model = model_ref_parts(&st.agent.model);
    let location = LocationRef::from_request(&query, &headers);
    Json(super::location::response_at(
        &st,
        &location,
        vec![AgentInfo {
            id: st.agent.name.to_string(),
            model: AgentModelRef {
                id: model.model_id,
                provider_id: model.provider_id,
            },
            request: RequestInfo {
                headers: BTreeMap::new(),
                body: json!({}),
            },
            system: st.agent.system_prompt.clone(),
            mode: "primary",
            hidden: false,
            permissions: super::agent_permission::from_engine(&st.engine),
        }],
    ))
}

async fn command(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<super::command_catalog::CommandInfo>>> {
    let location = LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Json(super::location::response_at(
        &st,
        &location,
        super::command_catalog::list(&workdir),
    ))
}

async fn skill(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<SkillInfo>>> {
    let location = LocationRef::from_request(&query, &headers);
    Json(super::location::response_at(
        &st,
        &location,
        discover_skills(&super::location::workdir_at(&st, &location)),
    ))
}

fn discover_skills(workdir: &Path) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    for dir in skill_dirs(workdir) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some((name, description, body)) = parse_skill(&content) {
                skills.push(SkillInfo {
                    name,
                    description,
                    location: path.to_string_lossy().into_owned(),
                    content: body,
                });
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn parse_skill(content: &str) -> Option<(String, String, String)> {
    let (frontmatter, body) = content.strip_prefix("---")?.split_once("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some((
        name?,
        description?,
        body.strip_prefix('\n').unwrap_or(body).to_string(),
    ))
}

fn skill_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".yaca/skills")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/yaca/skills"));
    }
    dirs
}
