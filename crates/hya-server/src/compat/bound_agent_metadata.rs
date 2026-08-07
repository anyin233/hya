//! Map the single bound BundleCatalog into Compat agent metadata rows.
//!
//! Not a second catalog authority: binds once per request workdir and projects
//! role → mode and can_spawn reachability → wire `hidden` from that catalog only.
//!
//! Also owns the sole remaining workdir `default_agent` config reader used by
//! session create and list sorting — no agent definition merge.

use std::path::{Path, PathBuf};

use hya_bundle::BundleError;
use hya_core::CoreError;
use hya_proto::AgentName;
use serde::Deserialize;

use crate::{ApiError, ServerState};

/// Shared projection used by `/api/agent` and legacy `/agent`.
pub(super) struct BoundAgentRow {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: String,
    pub(super) hidden: bool,
    pub(super) color: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) model: Option<String>,
}

/// Only the approved config key. Inline agent/permissions/options/model fields
/// are intentionally not deserialized.
#[derive(Default, Deserialize)]
struct DefaultAgentConfig {
    default_agent: Option<String>,
}

/// Capture one workdir `TurnBinding` and exact-resolve a root-session agent id.
///
/// When `requested` is present, that id is used. When omitted, the root default
/// is chosen in order: workdir `opencode.json` `default_agent`,
/// `ServerState.default_agent`, then `st.agent.name`. The candidate is
/// exact-resolved in that binding — no `general` fallback and no role gate.
/// Unknown ids surface as `BundleError::UnknownAgentId` via `CoreError`/`ApiError`.
pub(crate) async fn resolve_session_agent(
    st: &ServerState,
    workdir: &Path,
    requested: Option<&str>,
) -> Result<AgentName, ApiError> {
    let binding = st.engine.bind_root_runtime(workdir).await?;
    let candidate = match requested {
        Some(id) => id.to_string(),
        None => configured_default_agent(workdir)
            .or_else(|| st.default_agent.clone())
            .unwrap_or_else(|| st.agent.name.as_str().to_string()),
    };
    let agent = binding.resolve_agent(&candidate).ok_or_else(|| {
        ApiError::from(CoreError::from(BundleError::UnknownAgentId {
            agent_id: candidate,
        }))
    })?;
    Ok(AgentName::new(agent.stable_id))
}

/// Bind once for `workdir` and list catalog agents from that TurnBinding.
///
/// Bind failures surface as typed `ApiError` (via `CoreError`) rather than an
/// empty list fallback — there is no second authority when binding fails.
pub(super) async fn list(st: &ServerState, workdir: &Path) -> Result<Vec<BoundAgentRow>, ApiError> {
    let binding = st.engine.bind_root_runtime(workdir).await?;
    let catalog = binding.agent_catalog();

    // Ordinary reachability: every non-reserved agent is reachable, because
    // built-ins spawn the whole ordinary set. Reserved system agents are not.
    let mut rows: Vec<BoundAgentRow> = catalog
        .all()
        .into_iter()
        .map(|agent| {
            let name = agent.stable_id;
            // Role is the sole selector rule (main → primary).
            let mode = agent.selector_mode().to_string();
            // Wire `hidden` preserves autocomplete exclusion for unreachable
            // subagents; it never affects the TUI selector.
            let hidden = mode == "subagent" && catalog.is_reserved(name);
            BoundAgentRow {
                name: name.to_string(),
                description: agent.description.map(str::to_string),
                mode,
                hidden,
                color: agent.color.map(str::to_string),
                prompt: agent.prompt.map(str::to_string),
                model: agent.model_policy.model.clone(),
            }
        })
        .collect();

    // default_agent config value only — never merge agent definitions.
    let configured = configured_default_agent(workdir).or_else(|| st.default_agent.clone());
    sort_rows(&mut rows, configured.as_deref());
    Ok(rows)
}

/// Read `default_agent` from the four project config locations in discovery order.
///
/// Order (later matching file wins when it sets the key):
/// 1. `{workdir}/opencode.json`
/// 2. `{workdir}/opencode.jsonc`
/// 3. `{workdir}/.opencode/opencode.json`
/// 4. `{workdir}/.opencode/opencode.jsonc`
///
/// Uses the existing jsonc parser. Missing/unreadable/invalid files are skipped.
/// Only the `default_agent` key is considered — no agent definitions, permissions,
/// options, model, or reasoning fields are read.
pub(super) fn configured_default_agent(workdir: &Path) -> Option<String> {
    let mut default_agent = None;
    for path in project_config_paths(workdir) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(config) = super::jsonc::from_str::<DefaultAgentConfig>(&content) else {
            continue;
        };
        if config.default_agent.is_some() {
            default_agent = config.default_agent;
        }
    }
    default_agent
}

/// Promote a configured default to the front only when it is a role-main row.
/// Invalid or subagent defaults keep pure name order (fail-closed).
fn sort_rows(agents: &mut [BoundAgentRow], configured_default: Option<&str>) {
    agents.sort_by(|left, right| {
        let left_default = is_promoted_default(left, configured_default);
        let right_default = is_promoted_default(right, configured_default);
        right_default
            .cmp(&left_default)
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn is_promoted_default(agent: &BoundAgentRow, configured_default: Option<&str>) -> bool {
    match configured_default {
        Some(name) => agent.name == name && agent.mode == "primary",
        None => false,
    }
}

fn project_config_paths(workdir: &Path) -> [PathBuf; 4] {
    [
        workdir.join(super::external_protocol::CONFIG_FILE_JSON),
        workdir.join(super::external_protocol::CONFIG_FILE_JSONC),
        workdir
            .join(super::external_protocol::PROJECT_CONFIG_DIR)
            .join(super::external_protocol::CONFIG_FILE_JSON),
        workdir
            .join(super::external_protocol::PROJECT_CONFIG_DIR)
            .join(super::external_protocol::CONFIG_FILE_JSONC),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-server-default-agent-cfg-{nanos}-{serial}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Narrow characterization: among the four project config paths, the last
    /// file that successfully sets `default_agent` wins.
    #[test]
    fn later_matching_project_config_wins_default_agent() {
        let workdir = tempdir();
        std::fs::create_dir_all(workdir.join(".opencode")).unwrap();
        std::fs::write(
            workdir.join("opencode.json"),
            r#"{ "default_agent": "from-root-json", "agent": { "ghost": {} } }"#,
        )
        .unwrap();
        std::fs::write(
            workdir.join("opencode.jsonc"),
            // JSONC comment + trailing comma must parse; agent block ignored.
            r#"{
  // later than opencode.json
  "default_agent": "from-root-jsonc",
  "permissions": [{ "action": "read", "resource": "*", "effect": "deny" }],
}"#,
        )
        .unwrap();
        std::fs::write(
            workdir.join(".opencode/opencode.json"),
            r#"{ "default_agent": "from-nested-json", "mode": { "triage": {} } }"#,
        )
        .unwrap();
        std::fs::write(
            workdir.join(".opencode/opencode.jsonc"),
            r#"{ "default_agent": "from-nested-jsonc", "options": { "reasoningEffort": "high" } }"#,
        )
        .unwrap();

        assert_eq!(
            configured_default_agent(&workdir).as_deref(),
            Some("from-nested-jsonc")
        );

        // Unset in the last file does not clear an earlier match; only a present
        // key overwrites. Rewrite last file without the key → nested-json wins.
        std::fs::write(
            workdir.join(".opencode/opencode.jsonc"),
            r#"{ "provider": {} }"#,
        )
        .unwrap();
        assert_eq!(
            configured_default_agent(&workdir).as_deref(),
            Some("from-nested-json")
        );

        let _ = std::fs::remove_dir_all(&workdir);
    }
}
