//! App-layer [`hya_runtime_abi::HostRuntime`] implementation injected into
//! ABI v2 native bundles (`hya/coding-extras`, `hya/hashline`, etc.).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{Bundle, PreparedAgent};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, Message, ModelRef, Projection, SessionId,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, HostBundleStorage,
    HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry, HostModelCaller,
    HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest, ModelCallResponse,
    ModelStreamChunk, ModelWindowInfo, SessionListFilter, SessionSummaryMeta, TurnOutcome,
    TurnSpawnRequest, TurnStartRequest, TurnStatus,
};
use hya_store::SessionStore;
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// [`HostRuntime`] backed by `hya-app`'s [`SessionStore`] and active `config.yaml` path.
pub(crate) struct AppHostRuntime {
    store: SessionStore,
    config_file: PathBuf,
}

impl AppHostRuntime {
    /// Construct a host runtime for native bundles.
    pub(crate) fn new(store: SessionStore, config_file: PathBuf) -> Self {
        Self { store, config_file }
    }
}

#[async_trait]
impl HostSessionStore for AppHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map(|projection| projection.last_seq > 0)
            .map_err(|error| HostError::Storage(error.to_string()))
    }

    async fn read_events(
        &self,
        _session: SessionId,
        _after_seq: Option<EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        Err(HostError::Unavailable(
            "read_events not implemented on AppHostRuntime".into(),
        ))
    }

    async fn append_events(
        &self,
        _session: SessionId,
        _claim: Option<ActorClaim>,
        _events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        Err(HostError::Unavailable(
            "append_events not implemented on AppHostRuntime".into(),
        ))
    }

    async fn append_extension_event(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
        _claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        Err(HostError::Unavailable(
            "append_extension_event not implemented on AppHostRuntime".into(),
        ))
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        let projection = self.read_projection(session).await?;
        Ok(projection.extensions.get(bundle).cloned())
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        let projection = self
            .store
            .read_projection_shared(session)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))?;
        if projection.last_seq == 0 {
            return Ok(None);
        }
        Ok(Some(SessionSummaryMeta {
            session,
            parent: projection.session.parent,
            agent: projection.session.agent.clone(),
            model: projection.session.model.clone(),
            workdir: projection.session.workdir.clone(),
            title: projection.session.title.clone(),
            project: None,
            archived: projection.session.archived.is_some(),
            metadata: Value::Null,
            event_count: projection.last_seq,
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Err(HostError::Unavailable(
            "list_sessions not implemented on AppHostRuntime".into(),
        ))
    }
}

#[async_trait]
impl HostBundleStorage for AppHostRuntime {
    async fn get_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        Ok(None)
    }

    async fn put_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
        _value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        Err(HostError::Unavailable(
            "bundle_storage not wired on AppHostRuntime".into(),
        ))
    }

    async fn delete_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn list_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn acquire_claim(
        &self,
        _request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable(
            "bundle_storage claims not wired on AppHostRuntime".into(),
        ))
    }

    async fn renew_claim(
        &self,
        _lease: &BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable(
            "bundle_storage claims not wired on AppHostRuntime".into(),
        ))
    }

    async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
        Ok(())
    }

    async fn read_claim(
        &self,
        _bundle: &str,
        _resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostTurnRunner for AppHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable(
            "turn_runner not wired on AppHostRuntime".into(),
        ))
    }

    async fn run_turn(
        &self,
        _request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable(
            "turn_runner not wired on AppHostRuntime".into(),
        ))
    }

    async fn spawn_session_and_run(
        &self,
        _request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable(
            "turn_runner not wired on AppHostRuntime".into(),
        ))
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "turn_runner not wired on AppHostRuntime".into(),
        ))
    }

    async fn cancel_turn(
        &self,
        _session: SessionId,
        _reason: Option<String>,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
        Ok(TurnStatus::Idle)
    }
}

#[async_trait]
impl HostModelCaller for AppHostRuntime {
    async fn complete(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Err(HostError::Unavailable(
            "model_caller not wired on AppHostRuntime".into(),
        ))
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable(
            "model_caller not wired on AppHostRuntime".into(),
        ))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn model_window(&self, _model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostEventBus for AppHostRuntime {
    fn publish(&self, _envelope: Envelope) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "event_bus not wired on AppHostRuntime".into(),
        ))
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Err(HostError::Unavailable(
            "event_bus not wired on AppHostRuntime".into(),
        ))
    }

    async fn publish_extension(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
    ) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "event_bus not wired on AppHostRuntime".into(),
        ))
    }
}

#[async_trait]
impl HostCatalogReader for AppHostRuntime {
    async fn list_agents(&self, _session: Option<SessionId>) -> Result<Vec<AgentDef>, HostError> {
        Ok(Vec::new())
    }

    async fn get_agent(
        &self,
        _name: &AgentName,
        _session: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError> {
        Ok(None)
    }

    async fn list_skills(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }

    async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }

    async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError> {
        Ok(crate::config::read_bundle_config_json(
            &self.config_file,
            bundle_id,
        ))
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl HostExtensionRegistry for AppHostRuntime {
    async fn list_extensions(
        &self,
        _kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn invoke_bundle_api(
        &self,
        _call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable(
            "extension_registry not wired on AppHostRuntime".into(),
        ))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable(
            "extension_registry not wired on AppHostRuntime".into(),
        ))
    }
}

impl HostRuntime for AppHostRuntime {
    fn session_store(&self) -> &dyn HostSessionStore {
        self
    }

    fn bundle_storage(&self) -> &dyn HostBundleStorage {
        self
    }

    fn turn_runner(&self) -> &dyn HostTurnRunner {
        self
    }

    fn model_caller(&self) -> &dyn HostModelCaller {
        self
    }

    fn event_bus(&self) -> &dyn HostEventBus {
        self
    }

    fn catalog_reader(&self) -> &dyn HostCatalogReader {
        self
    }

    fn extension_registry(&self) -> &dyn HostExtensionRegistry {
        self
    }
}
