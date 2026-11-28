//! Lockstep Native Runtime ABI v2 `cdylib` entry point for `hya/workflow`.

pub use hya_workflow_bundle_core::*;

hya_runtime_abi::export_native_bundle_v2!(|out| {
    hya_workflow_bundle_core::register_bundle(out);
});

#[cfg(test)]
mod tests {
    #[test]
    fn cdylib_registers_workflow_extensions() {
        let mut regs = hya_runtime_abi::NativeRegistrations::default();
        hya_workflow_bundle_core::register_bundle(&mut regs);
        assert_eq!(regs.services.len(), 1);
        assert_eq!(regs.tools.len(), 1);
        assert_eq!(regs.api_handlers.len(), 1);
        assert_eq!(regs.projection_reducers.len(), 1);
    }
}
