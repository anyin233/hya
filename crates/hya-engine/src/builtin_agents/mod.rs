//! The one compiled-in agent, `hya-bare`, and the well-known agent ids.
//!
//! Every other agent, `hya-main` included, comes from an enabled bundle through
//! [`crate::AgentCatalog`]; nothing else is compiled into the binary.

use std::sync::OnceLock;

use hya_bundle::{AgentRole, HandoffPolicy, ModelPolicy, PreparedAgent, ResourceView};
use hya_proto::AgentName;
use sha2::{Digest, Sha256};

/// Compiled-in primary agent: the default when no enabled bundle declares
/// `default_agent` (ADR-0040).
pub const BARE_AGENT_ID: &str = "hya-bare";

/// Default primary agent of the `hya/core-agents` bundle.
pub const MAIN_AGENT_ID: &str = "hya-main";

/// General-purpose worker: the `task` target when `subagent_type` is empty, and
/// the agent a stored session continues as when its recorded agent no longer
/// exists in the catalog.
pub const TASK_AGENT_ID: &str = "hya-task";

/// The compiled-in `hya-bare` agent.
#[must_use]
pub(crate) fn core_agent() -> &'static PreparedAgent {
    static AGENT: OnceLock<PreparedAgent> = OnceLock::new();
    AGENT.get_or_init(|| PreparedAgent {
        id: AgentName::new(BARE_AGENT_ID),
        description: Some("Minimal bare coding agent (standard kernel primitives)".to_string()),
        role: AgentRole::Main,
        color: None,
        prompt: None,
        prompt_source: None,
        prompt_digest: None,
        model_policy: ModelPolicy::default(),
        workdir: None,
        resource_view: ResourceView::default(),
        can_spawn: Vec::new(),
        private: false,
        advertise: Vec::new(),
        handoff: HandoffPolicy::default(),
        hook_refs: Vec::new(),
    })
}

/// Deterministic SHA-256 digest of the compiled-in `hya-bare` agent.
#[must_use]
pub(crate) fn core_agent_digest() -> &'static [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    DIGEST.get_or_init(|| Sha256::digest(b"hya/bare:core-agent:hya-bare:v1").into())
}
