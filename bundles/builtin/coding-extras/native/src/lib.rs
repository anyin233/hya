//! Lockstep Native Runtime ABI v2 `cdylib` entry point for `hya/coding-extras`.

pub use hya_coding_extras_core::*;

hya_runtime_abi::export_native_bundle_v2!(|ctx, out| {
    hya_coding_extras_core::register_bundle(ctx, out);
});

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    #[test]
    fn cdylib_exports_match_core_registrations() {
        let state = Arc::new(hya_coding_extras_core::CodingExtrasState::new());
        assert_eq!(hya_coding_extras_core::coding_extras_tools(state).len(), 7);
    }
}
