//! Native implementation of the `hya/loop` built-in bundle (`LoopDriver` and
//! `LoopService`) exported via Lockstep Native ABI v2.
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>`:
//! `HostTurnRunner`, `HostModelCaller`, `HostSessionStore`, `HostCatalogReader`,
//! `HostEventBus`, `HostBundleStorage`, and `HostExtensionRegistry`.

use std::sync::Arc;

/// Loop driver, worker session executor, and iteration runner.
pub mod driver;
/// Model-backed and hook-backed verifiers, planners, and stop hooks.
pub mod evaluators;
/// Verifier + planner + predicate [`LoopGate`].
pub mod gate;
/// Deterministic `--while` / `--until` shell exit predicates.
pub mod predicate;
/// Managed lifecycle service for `hya/loop`.
pub mod service;
/// Core loop domain types, traits, budget validation, and transcript rendering.
pub mod types;

pub use driver::{IterationDriver, NativeLoopDriver, WorkerSessionExecutor, drive_loop, run_loop};
pub use evaluators::{
    DEFAULT_LOOP_PLANNER_SYSTEM, DEFAULT_LOOP_VERIFIER_SYSTEM, HookLoopPlanner, HookLoopVerifier,
    ModelLoopPlanner, ModelLoopVerifier, RuntimeShouldStopHook,
};
pub use gate::LoopGate;
pub use predicate::{
    DEFAULT_PREDICATE_TIMEOUT_SECS, LoopPredicate, LoopPredicateOutcome, LoopPredicateSpec,
    PredicateMode,
};
pub use service::LoopService;
pub use types::{
    DEFAULT_LOOP_BUDGET, DEFAULT_MAX_WALL_CLOCK_SECS, EvidenceQuality, GateOutcome,
    HARD_MAX_ITERATIONS, IterationExecutor, IterationGate, LOOP_BUNDLE_ID, LOOP_DRIVER_ID,
    LOOP_SERVICE_ID, LoopConfig, LoopPlanner, LoopShouldStopHook, LoopVerifier, PlannerOutput,
    SafetyCaps, VerifierVerdict, clamp_budget, render_transcript, skill_prompt_body,
    validate_budget, wrap_up_directive,
};

hya_runtime_abi::export_native_bundle_v2!(|out| {
    let service = Arc::new(LoopService::new());
    out.services
        .push(Arc::clone(&service) as Arc<dyn hya_runtime_abi::NativeService>);
    out.loop_drivers
        .push(Arc::new(NativeLoopDriver::new(service)) as Arc<dyn hya_runtime_abi::LoopDriver>);
});

#[cfg(test)]
mod tests;
