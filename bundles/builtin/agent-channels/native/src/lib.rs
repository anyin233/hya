//! Lockstep Native Runtime ABI v2 `cdylib` entry point for `hya/agent-channels`.

pub use hya_agent_channels_core::*;

hya_runtime_abi::export_native_bundle_v2!(|out| {
    hya_agent_channels_core::register_bundle(out);
});

#[cfg(test)]
mod tests {
    #[test]
    fn cdylib_registers_agent_channels_extensions() {
        let mut regs = hya_runtime_abi::NativeRegistrations::default();
        hya_agent_channels_core::register_bundle(&mut regs);
        assert_eq!(regs.services.len(), 1);
        assert_eq!(regs.tools.len(), 4);
        assert_eq!(regs.uri_schema_handlers.len(), 1);
        assert_eq!(regs.hooks.len(), 1);
        assert_eq!(regs.api_handlers.len(), 1);
        assert_eq!(regs.projection_reducers.len(), 1);
    }
}
