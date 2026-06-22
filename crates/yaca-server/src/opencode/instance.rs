use std::path::{Path, PathBuf};

use axum::Json;
use axum::Router;
use axum::routing::{get, post};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ServerState;

mod vcs;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(vcs::router())
        .route("/instance/dispose", post(dispose))
        .route("/path", get(path))
        .route("/command", get(command))
        .route("/agent", get(agent))
        .route("/skill", get(skill))
        .route("/lsp", get(empty_array))
        .route("/formatter", get(empty_array))
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
    mode: &'static str,
    native: bool,
    permission: Vec<Value>,
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

#[derive(Serialize)]
struct CommandInfo {
    name: &'static str,
    description: &'static str,
    source: &'static str,
    template: &'static str,
    hints: Vec<&'static str>,
}

#[derive(Serialize)]
struct SkillInfo {
    name: String,
    description: String,
    location: String,
    content: String,
}

async fn dispose() -> Json<bool> {
    Json(true)
}

async fn path(axum::extract::State(st): axum::extract::State<ServerState>) -> Json<PathInfo> {
    let workdir = workdir(&st);
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
        mode: "primary",
        native: true,
        permission: Vec::new(),
        model: model_info(st.agent.model.as_str()),
        prompt: st.agent.system_prompt.clone(),
        options: json!({}),
    }])
}

async fn command() -> Json<Vec<CommandInfo>> {
    Json(vec![
        command_info("help", "show this help", "/help", Vec::new()),
        command_info(
            "model",
            "switch the active model",
            "/model $ARGUMENTS",
            vec!["$ARGUMENTS"],
        ),
        command_info("clear", "start a fresh session", "/clear", Vec::new()),
        command_info(
            "sessions",
            "switch to another session",
            "/sessions",
            Vec::new(),
        ),
        command_info(
            "yolo",
            "toggle auto-approval",
            "/yolo $ARGUMENTS",
            vec!["$ARGUMENTS"],
        ),
        command_info(
            "think",
            "set reasoning effort",
            "/think $ARGUMENTS",
            vec!["$ARGUMENTS"],
        ),
    ])
}

async fn skill(
    axum::extract::State(st): axum::extract::State<ServerState>,
) -> Json<Vec<SkillInfo>> {
    Json(discover_skills(&workdir(&st)))
}

async fn empty_array() -> Json<Vec<Value>> {
    Json(Vec::new())
}

fn command_info(
    name: &'static str,
    description: &'static str,
    template: &'static str,
    hints: Vec<&'static str>,
) -> CommandInfo {
    CommandInfo {
        name,
        description,
        source: "command",
        template,
        hints,
    }
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
            if let Some((name, description)) = parse_skill(&content) {
                skills.push(SkillInfo {
                    name,
                    description,
                    location: path.to_string_lossy().into_owned(),
                    content,
                });
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn parse_skill(content: &str) -> Option<(String, String)> {
    let frontmatter = content.strip_prefix("---")?.split_once("\n---")?.0;
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some((name?, description?))
}

fn skill_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".yaca/skills")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/yaca/skills"));
    }
    dirs
}

pub(super) fn workdir(st: &ServerState) -> PathBuf {
    match std::fs::canonicalize(&st.agent.workdir) {
        Ok(path) => path,
        Err(_) => st.agent.workdir.clone(),
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
