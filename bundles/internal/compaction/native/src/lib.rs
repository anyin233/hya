//! `hya/compaction`: full mode's context compaction (ADR-0033), an internal
//! native bundle. Its [`CompactionRewriter`] walks the reduction ladder before
//! each round and folds the session on `/summarize`, appending every change
//! as events through the host's session log.

mod config;
mod evict;
mod fold;
mod ladder;
mod snap;

use std::sync::Arc;

use async_trait::async_trait;
use hya_tool::{RewriteCtx, Rewriter, Tool, ToolError};

/// The compaction ladder as a [`Rewriter`].
pub struct CompactionRewriter;

#[async_trait]
impl Rewriter for CompactionRewriter {
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        ladder::walk(&cx).await
    }

    async fn rewrite_now(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        ladder::summarize_now(&cx).await
    }
}

/// Write this library's lockstep ABI digest to `out`.
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

/// Register this bundle's tools: it has none.
///
/// # Safety
///
/// `out` must point to a live `Vec<Arc<dyn Tool>>` built with the matching
/// hya-tool ABI, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_v1(_out: *mut Vec<Arc<dyn Tool>>) {}

/// Register the compaction rewriter.
///
/// # Safety
///
/// `out` must point to a live `Vec<Arc<dyn Rewriter>>` built with the matching
/// hya-tool ABI, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_register_rewriters_v1(out: *mut Vec<Arc<dyn Rewriter>>) {
    // SAFETY: the host calls this only after the ABI digest matches.
    if let Some(out) = unsafe { out.as_mut() } {
        out.push(Arc::new(CompactionRewriter));
    }
}

/// Poll one native future while this library's Tokio runtime TLS is entered.
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

/// Read the host's run deadline from this library's copy of `hya-tool`.
///
/// # Safety
///
/// `cell` must be the host's deadline cell from the matching lockstep build.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_share_deadline_v1(cell: *const std::ffi::c_void) {
    // SAFETY: the checked host passes its 'static cell.
    unsafe { hya_tool::native_bundle::share_deadline_v1(cell) };
}
