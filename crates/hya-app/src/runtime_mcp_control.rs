use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hya_engine::RuntimeSourceKind;
use hya_mcp::{McpServerConfig, McpStatus};
use serde_json::Value;

use crate::runtime_reconcile::{
    ObservedState, PreparedFailure, PreparedResult, ReconcileError, ReconcileOutcome,
    ReconcilePlan, RuntimeReconciler, prepare_desired_source,
};

type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) struct RuntimeMcpControl {
    reconciler: Arc<RuntimeReconciler>,
}

impl RuntimeMcpControl {
    #[must_use]
    pub(crate) fn new(reconciler: Arc<RuntimeReconciler>) -> Self {
        Self { reconciler }
    }

    pub(crate) async fn reconcile_plan(
        &self,
        mut plan: ReconcilePlan,
        stop: tokio_util::sync::CancellationToken,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        if plan.is_complete() {
            return Ok(ReconcileOutcome {
                published_generation: None,
            });
        }
        let mut set = tokio::task::JoinSet::new();
        let mut tasks = BTreeMap::new();
        for source in plan.sources().iter().cloned() {
            let id = source.id().clone();
            let handle = set.spawn(hya_child_io::io_tasks::scope(
                self.reconciler.io_tasks.clone(),
                async move { prepare_desired_source(source).await },
            ));
            tasks.insert(handle.id(), id);
        }
        let mut first_error = None;
        let mut published_generation = None;
        loop {
            let joined = tokio::select! {
                biased;
                () = stop.cancelled() => {
                    set.abort_all();
                    while set.join_next().await.is_some() {}
                    return Err(ReconcileError::InvalidPrepared("runtime reconciliation cancelled".to_string()));
                }
                joined = set.join_next_with_id() => match joined {
                    Some(joined) => joined,
                    None => break,
                },
            };
            let result = match joined {
                Ok((id, result)) => {
                    tasks.remove(&id);
                    result
                }
                Err(error) => {
                    let Some(source) = tasks.remove(&error.id()) else {
                        return Err(ReconcileError::InvalidPrepared(format!(
                            "source preparation task {} had no source ticket",
                            error.id()
                        )));
                    };
                    PreparedResult::Failed(PreparedFailure::new(
                        source,
                        format!("SOURCE_PREPARE_TASK_FAILED: {error}"),
                    ))
                }
            };
            let id = result.source().clone();
            let ticket = plan.tickets.remove(&id).ok_or_else(|| {
                ReconcileError::InvalidPrepared(format!("missing planned ticket for {id}"))
            })?;
            let source_plan = ReconcilePlan {
                tickets: BTreeMap::from([(id, ticket)]),
                sources: Vec::new(),
            };
            match self.reconciler.finish_revision(&source_plan, [result]) {
                Ok(outcome) => {
                    if outcome.published_generation.is_some() {
                        published_generation = outcome.published_generation;
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(ReconcileOutcome {
                published_generation,
            }),
        }
    }

    fn mcp_status(&self) -> BTreeMap<String, McpStatus> {
        self.reconciler
            .status()
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .map(|(source, status)| {
                let declaration_matches = status.observed_declaration_digest.is_some()
                    && status.observed_declaration_digest == status.effective_declaration_digest;
                let value = match status.observed {
                    ObservedState::Connecting => McpStatus::Connecting,
                    ObservedState::Ready if status.effective && declaration_matches => {
                        McpStatus::Connected
                    }
                    ObservedState::Failed => McpStatus::Failed {
                        error: status
                            .typed_error
                            .unwrap_or_else(|| "MCP reconciliation failed".to_string()),
                    },
                    ObservedState::Removed if status.desired => McpStatus::Disabled,
                    ObservedState::Ready | ObservedState::Removed => McpStatus::Failed {
                        error: format!(
                            "MCP source is not effective in generation {}",
                            status.effective_generation.get()
                        ),
                    },
                };
                (source.configured_id().to_string(), value)
            })
            .collect()
    }

    fn effective_tools(&self) -> BTreeMap<String, Vec<String>> {
        self.reconciler
            .registry
            .effective_manifest()
            .sources
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .map(|(source, manifest)| (source.configured_id().to_string(), manifest.exports))
            .collect()
    }

    fn effective_resources(&self) -> BTreeMap<String, Value> {
        self.reconciler
            .registry
            .effective_manifest()
            .sources
            .into_iter()
            .filter(|(source, _)| source.kind() == RuntimeSourceKind::Mcp)
            .flat_map(|(_, source)| {
                source
                    .resources
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl hya_server::McpControl for RuntimeMcpControl {
    fn status(&self) -> ControlFuture<'_, BTreeMap<String, McpStatus>> {
        Box::pin(async move { self.mcp_status() })
    }

    fn upsert(
        &self,
        name: String,
        config: McpServerConfig,
    ) -> ControlFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let plan = self
                .reconciler
                .upsert_mcp(name, config)
                .map_err(|error| error.to_string())?;
            match self
                .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
                .await
            {
                Ok(outcome) => {
                    trace_publication(&outcome);
                    Ok(())
                }
                // The server is stored and its status is FAILED with this
                // error; the add itself succeeded.
                Err(ReconcileError::PreparationFailed { .. }) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn set_enabled(&self, name: String, enabled: bool) -> ControlFuture<'_, Result<bool, String>> {
        Box::pin(async move {
            let Some(plan) = self
                .reconciler
                .set_mcp_enabled(&name, enabled)
                .map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            match self
                .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
                .await
            {
                Ok(outcome) => {
                    trace_publication(&outcome);
                    Ok(true)
                }
                // A connect failure is the server's FAILED status.
                Err(ReconcileError::PreparationFailed { .. }) => Ok(true),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn resources(&self) -> ControlFuture<'_, BTreeMap<String, Value>> {
        Box::pin(async move { self.effective_resources() })
    }

    fn tools(&self) -> ControlFuture<'_, BTreeMap<String, Vec<String>>> {
        Box::pin(async move { self.effective_tools() })
    }
}

fn trace_publication(outcome: &ReconcileOutcome) {
    if let Some(generation) = outcome.published_generation {
        tracing::debug!(
            generation = generation.get(),
            "published reconciled runtime sources"
        );
    }
}

#[cfg(test)]
mod tests;
