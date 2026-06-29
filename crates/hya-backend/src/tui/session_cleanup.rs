use std::sync::Arc;

use anyhow::Context as _;
use hya_core::SessionEngine;
use hya_proto::SessionId;

pub(super) async fn cleanup_current_session_for_finalization(
    engine: &Arc<SessionEngine>,
    session: SessionId,
) -> anyhow::Result<bool> {
    engine
        .cleanup_empty_unnamed_session(session)
        .await
        .context("cleanup empty unnamed session")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hya_core::{AgentSpec, CreateSession, EventBus};
    use hya_proto::{AgentName, ModelRef};
    use hya_provider::ProviderRouter;
    use hya_store::SessionStore;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    async fn cleanup_test_engine() -> Arc<SessionEngine> {
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new());
        let tools = Arc::new(ToolRegistry::builtins());
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        Arc::new(SessionEngine::new(
            store,
            router,
            tools,
            permission,
            EventBus::default(),
        ))
    }

    fn cleanup_test_spec(agent: &AgentSpec) -> CreateSession {
        CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        }
    }

    #[tokio::test]
    async fn cleanup_empty_unnamed_session_on_exit_deletes_empty_current_session() {
        let engine = cleanup_test_engine().await;
        let agent = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: String::new(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        };
        let session = engine.create(cleanup_test_spec(&agent)).await.unwrap();

        cleanup_current_session_for_finalization(&engine, session)
            .await
            .unwrap();

        assert!(engine.replay(session).await.unwrap().is_empty());
    }
}
