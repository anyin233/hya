//! Lockstep Native Runtime ABI v2 `cdylib` entry point for `hya/multi-agent`.

pub use hya_multi_agent_core::*;

hya_runtime_abi::export_native_bundle_v2!(|out| {
    hya_multi_agent_core::register_bundle(out);
});

#[cfg(test)]
mod tests {
    #[test]
    fn cdylib_registers_multi_agent_extensions() {
        let mut regs = hya_runtime_abi::NativeRegistrations::default();
        hya_multi_agent_core::register_bundle(&mut regs);
        assert_eq!(regs.services.len(), 1);
        assert_eq!(regs.tools.len(), 5);
        assert_eq!(regs.hooks.len(), 1);
        assert_eq!(regs.api_handlers.len(), 1);
        assert_eq!(regs.projection_reducers.len(), 1);
    }
}
