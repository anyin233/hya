//! Integration tests for `hya-core`, built as one binary; each module covers one area.

mod kernel;
mod replay_bare;

#[test]
fn internal_native_bundle_loads_its_rewriter() -> Result<(), hya_core::CoreError> {
    let registrations = hya_core::bundles::load_internal()?;
    assert!(registrations.tools.is_empty());
    assert_eq!(registrations.rewriters.len(), 1);
    Ok(())
}
