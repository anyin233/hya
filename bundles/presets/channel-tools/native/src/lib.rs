//! Native implementation of the channel built-in tool family.

use std::sync::Arc;

use hya_tool::Tool;

mod mailbox_tools;
mod report;

/// Report the lockstep Rust tool ABI before any Rust object crosses the library boundary.
///
/// # Safety
///
/// `out` must point to a writable array of at least 32 bytes, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_abi_v1(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let digest = hya_tool::native_bundle::abi_digest_v1();
    // SAFETY: the caller supplies a writable 32-byte output array.
    unsafe { std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len()) };
}

/// Register this bundle's concrete tools with the host's tool registry loader.
///
/// # Safety
///
/// `out` must point to a live `Vec<Arc<dyn Tool>>` built with the matching
/// hya-tool ABI, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_v1(out: *mut Vec<Arc<dyn Tool>>) {
    // SAFETY: the host calls this only after the ABI digest matches.
    if let Some(out) = unsafe { out.as_mut() } {
        let tools: [Arc<dyn Tool>; 3] = [
            Arc::new(mailbox_tools::SendTool),
            Arc::new(mailbox_tools::ListChannelTool),
            Arc::new(report::ReportTool),
        ];
        out.extend(tools);
    }
}

/// Poll one native tool future while this library's Tokio runtime TLS is entered.
///
/// # Safety
///
/// `handle` and `state` must remain valid throughout the synchronous callback;
/// the handle must come from the matching host Tokio build.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_with_runtime_v1(
    handle: *const std::ffi::c_void,
    callback: unsafe extern "C" fn(*mut std::ffi::c_void),
    state: *mut std::ffi::c_void,
) {
    // SAFETY: the checked host ABI guarantees the live handle and callback.
    unsafe { hya_tool::native_bundle::with_runtime_v1(handle, callback, state) };
}
