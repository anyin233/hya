//! Loading the internal native bundles (ADR-0033, D9): the full-mode features
//! that run outside the kernel. Bare mode never calls this.

use hya_tool::native_bundle::{self, Registrations};

use crate::error::CoreError;

/// Load every bundle in [`hya_bundle::INTERNAL_BUNDLES`], in order, and
/// collect their registrations.
///
/// # Errors
///
/// Returns [`CoreError::BundleRuntime`] for the first bundle whose library
/// cannot be found, verified, or loaded.
pub fn load_internal() -> Result<Registrations, CoreError> {
    let mut all = Registrations {
        tools: Vec::new(),
        rewriters: Vec::new(),
    };
    for identity in hya_bundle::INTERNAL_BUNDLES {
        let loaded = native_bundle::load(identity).map_err(|error| CoreError::BundleRuntime {
            bundle_id: (*identity).to_string(),
            source: Box::new(CoreError::Invalid(error)),
        })?;
        all.tools.extend(loaded.tools);
        all.rewriters.extend(loaded.rewriters);
    }
    Ok(all)
}
