//! Bridge from [`TurnBinding`] onto [`hya_agent_channels_core::ChannelPolicy`].

use hya_bundle::BundleError;

pub use hya_agent_channels_core::{AGENT_CHANNELS_PRESET_ID, ChannelPolicy};

use crate::TurnBinding;

/// Compile channel policy from the immutable catalog retained by an admitted turn.
///
/// # Errors
/// Returns [`BundleError`] if the trusted `hya/agent-channels` defaults manifest
/// cannot be decoded.
pub fn from_binding(binding: &TurnBinding) -> Result<ChannelPolicy, BundleError> {
    if binding.agent_catalog().is_bare() {
        return Ok(ChannelPolicy::empty());
    }
    ChannelPolicy::from_bundle_catalog(binding.bundle_catalog())
}
