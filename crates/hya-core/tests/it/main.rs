//! Integration tests for `hya-core`, built as one binary; each module covers one area.

mod kernel;
mod reasoning_deadline_guard;
mod replay_bare;
mod standard_tools;

#[test]
fn compaction_bundle_loads_its_rewriter_and_projection_reducer()
-> Result<(), hya_runtime_abi::AbiError> {
    let registrations = hya_tool_loader::load_v2(
        "hya/compaction",
        std::sync::Arc::new(hya_tool_loader::MinimalHostRuntime::empty()),
    )?;
    assert!(registrations.tools.is_empty());
    assert_eq!(registrations.rewriters.len(), 1);
    assert_eq!(registrations.projection_reducers.len(), 1);
    assert_eq!(
        registrations.projection_reducers[0].bundle_id(),
        "hya/compaction"
    );
    Ok(())
}
