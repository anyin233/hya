//! Test-only replacement for the TODO family, built as a real shared library.
use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use std::sync::Arc;

struct Replacement(&'static str);
#[async_trait]
impl Tool for Replacement {
    fn name(&self) -> &str {
        self.0
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "replacement fixture".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }
    async fn execute(&self, _: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        Ok(json!({"implementation":"replacement", "input":input}))
    }
}

/// # Safety
/// `out` points to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_abi_v1(out: *mut u8) {
    let digest = hya_tool::native_bundle::abi_digest_v1();
    unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), out, 32) };
}
/// # Safety
/// `out` is a live vector from the matching host build.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_v1(out: *mut Vec<Arc<dyn Tool>>) {
    let out = unsafe { &mut *out };
    for name in ["todo__read", "todo__update_status", "todo__update_content"] {
        out.push(Arc::new(Replacement(name)));
    }
}
/// # Safety
/// The handle and callback state satisfy `with_runtime_v1`'s contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_with_runtime_v1(
    handle: *const std::ffi::c_void,
    callback: unsafe extern "C" fn(*mut std::ffi::c_void),
    state: *mut std::ffi::c_void,
) {
    unsafe { hya_tool::native_bundle::with_runtime_v1(handle, callback, state) };
}
