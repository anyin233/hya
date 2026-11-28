//! Loading the internal native bundles (ADR-0033, ADR-0039): the full-mode
//! features that run outside the kernel over lockstep ABI v2. Bare mode never
//! calls this.

use std::sync::Arc;

use hya_runtime_abi::{HostRuntime, NativeRegistrations};
use hya_tool_loader::MinimalHostRuntime;

use crate::error::CoreError;

/// Load every bundle in [`hya_bundle::INTERNAL_BUNDLES`] with a default
/// [`MinimalHostRuntime`], in order, and collect their ABI v2 registrations.
///
/// # Errors
///
/// Returns [`CoreError::BundleRuntime`] for the first bundle whose library
/// cannot be found, verified, or loaded.
pub fn load_internal() -> Result<NativeRegistrations, CoreError> {
    load_internal_with_runtime(Arc::new(MinimalHostRuntime::empty()))
}

/// Load every bundle in [`hya_bundle::INTERNAL_BUNDLES`] with the given
/// [`HostRuntime`], in order, and collect their ABI v2 registrations.
///
/// # Errors
///
/// Returns [`CoreError::BundleRuntime`] for the first bundle whose library
/// cannot be found, verified, or loaded.
pub fn load_internal_with_runtime(
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, CoreError> {
    let mut all = NativeRegistrations::new();
    for identity in hya_bundle::INTERNAL_BUNDLES {
        let loaded = hya_tool_loader::load_v2(identity, Arc::clone(&runtime)).map_err(|error| {
            CoreError::BundleRuntime {
                bundle_id: (*identity).to_string(),
                source: Box::new(CoreError::Invalid(error.to_string())),
            }
        })?;
        all.extend(loaded);
    }
    Ok(all)
}
