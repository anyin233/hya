//! Generic explicit model-route state for turns with ordered fallback candidates.

use std::sync::{Arc, Mutex};

use hya_proto::ModelRef;
use hya_provider::{ProviderError, ReasoningEffort};
use hya_runtime_abi::{TurnRouteFailureClass, TurnRouteOutcome};

use crate::error::CoreError;

/// One runtime-resolved candidate in an explicit turn model route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplicitModelRouteCandidate {
    /// Base model reference (never carries a `#effort` suffix).
    pub model: ModelRef,
    /// Effective typed reasoning effort for this candidate.
    pub reasoning: ReasoningEffort,
}

/// Immutable ordered model route selected before turn execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplicitModelRoute {
    /// Full declaration-order candidates.
    pub candidates: Arc<[ExplicitModelRouteCandidate]>,
    /// Index of the first routable candidate at admission time.
    pub selected_index: usize,
}

impl ExplicitModelRoute {
    /// Return the candidate selected at admission time.
    #[must_use]
    pub fn selected(&self) -> Option<&ExplicitModelRouteCandidate> {
        self.candidates.get(self.selected_index)
    }
}

#[derive(Default)]
struct ExplicitRouteState {
    step: u32,
    candidate_index: Option<usize>,
    pending_failure: Option<TurnRouteFailureClass>,
    outcomes: Vec<TurnRouteOutcome>,
}

/// Request-local explicit model route and exactly-once outcome collector.
#[derive(Clone)]
pub struct ExplicitTurnRoute {
    route: ExplicitModelRoute,
    state: Arc<Mutex<ExplicitRouteState>>,
}

impl ExplicitTurnRoute {
    /// Wrap an immutable [`ExplicitModelRoute`] for one turn activation.
    #[must_use]
    pub fn new(route: ExplicitModelRoute) -> Self {
        Self {
            route,
            state: Arc::new(Mutex::new(ExplicitRouteState::default())),
        }
    }

    /// Borrow the underlying immutable candidate route.
    #[must_use]
    pub fn route(&self) -> &ExplicitModelRoute {
        &self.route
    }

    /// Return all finalized [`TurnRouteOutcome`] records collected so far.
    #[must_use]
    pub fn outcomes(&self) -> Vec<TurnRouteOutcome> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .outcomes
            .clone()
    }

    /// Mark the candidate attempt before awaiting provider transport.
    pub(crate) fn begin_attempt(&self, candidate_index: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.candidate_index.is_none() {
            state.pending_failure = None;
        }
        state.candidate_index = Some(candidate_index);
    }

    /// Record the failure class that caused a forward pre-stream advance.
    pub(crate) fn record_failure(&self, candidate_index: usize, failure: TurnRouteFailureClass) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.candidate_index = Some(candidate_index);
        state.pending_failure = Some(failure);
    }

    /// Mark a returned stream and retain the class that caused its final advance.
    pub(crate) fn selected(
        &self,
        candidate_index: usize,
        pending_failure: Option<TurnRouteFailureClass>,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.candidate_index = Some(candidate_index);
        state.pending_failure = pending_failure;
    }

    /// Finalize at most one bounded outcome for the current stream group.
    pub(crate) fn finalize(&self, failure: Option<TurnRouteFailureClass>) -> Result<(), CoreError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(candidate_index) = state.candidate_index.take() else {
            return Ok(());
        };
        let step = state.step;
        state.step = state.step.saturating_add(1);
        let pending_failure = state.pending_failure.take();
        let Some(candidate) = self.route.candidates.get(candidate_index) else {
            return Err(CoreError::Invalid(
                "explicit route selected candidate is out of bounds".to_string(),
            ));
        };
        state.outcomes.push(TurnRouteOutcome {
            step,
            candidate_index: u32::try_from(candidate_index).unwrap_or(u32::MAX),
            model: candidate.model.clone(),
            reasoning: candidate.reasoning.as_str().to_string(),
            failure_class: failure
                .or(pending_failure)
                .unwrap_or(TurnRouteFailureClass::None),
        });
        Ok(())
    }
}

/// Classify a [`ProviderError`] into a stable [`TurnRouteFailureClass`].
#[must_use]
pub(crate) fn provider_route_failure_class(error: &ProviderError) -> TurnRouteFailureClass {
    use TurnRouteFailureClass as Class;
    match error {
        ProviderError::Transport(_) => Class::Transport,
        ProviderError::HttpStatus { status, .. } => {
            if *status == 429 {
                Class::RateLimited
            } else if (500..=599).contains(status) {
                Class::Server
            } else {
                Class::Http
            }
        }
        ProviderError::UnknownModel(_) => Class::UnknownModel,
        ProviderError::AuthExpired { .. } => Class::Auth,
        ProviderError::Incompatible(_) => Class::Incompatible,
        ProviderError::Decode(_) => Class::Decode,
        ProviderError::Json(_) | ProviderError::Http(_) | ProviderError::StreamInterrupted(_) => {
            Class::Http
        }
    }
}

/// Classify a [`CoreError`] into a stable [`TurnRouteFailureClass`].
#[must_use]
pub(crate) fn core_route_failure_class(
    error: &CoreError,
    cancel: &tokio_util::sync::CancellationToken,
) -> TurnRouteFailureClass {
    match error {
        CoreError::Provider(error) => provider_route_failure_class(error),
        CoreError::Cancelled if cancel.is_cancelled() => TurnRouteFailureClass::Cancelled,
        CoreError::Cancelled => TurnRouteFailureClass::Aborted,
        _ => TurnRouteFailureClass::Internal,
    }
}
