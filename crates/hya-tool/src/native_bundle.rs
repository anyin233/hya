//! Lockstep Rust ABI shared by the host and first-party native libraries.

use std::sync::{Arc, OnceLock};

use hya_context::rewrite::Rewriter;
use sha2::{Digest, Sha256};

use crate::{Tool, ToolCtx, ToolError};

/// Enter the bundle's Tokio runtime while polling one native tool future.
///
/// Native libraries export a C wrapper that calls this function. The callback
/// must poll one tool future synchronously before returning. The host runtime
/// and its task-local context remain active; the bundle runtime drives its own I/O.
/// A failed bundle runtime build falls back to the checked host handle.
///
/// # Safety
///
/// `handle` must point to a live `tokio::runtime::Handle` from the matching
/// lockstep build, and `callback` must accept the live `state` pointer without
/// retaining it after this function returns.
pub unsafe fn with_runtime_v1(
    handle: *const libc::c_void,
    callback: unsafe extern "C" fn(*mut libc::c_void),
    state: *mut libc::c_void,
) {
    static BUNDLE_RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    let runtime = BUNDLE_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .ok()
    });
    if let Some(runtime) = runtime {
        let _guard = runtime.enter();
        // SAFETY: caller guarantees `state` is live for this synchronous call.
        unsafe { callback(state) };
    } else {
        // SAFETY: the host passes a live Handle whose ABI was checked before
        // invoking this export.
        let handle = unsafe { &*handle.cast::<tokio::runtime::Handle>() };
        let _guard = handle.enter();
        // SAFETY: caller guarantees `state` is live for this synchronous call.
        unsafe { callback(state) };
    }
}

/// Share the host's run deadline and unbudgeted job registry with this bundle.
///
/// Native libraries export a C wrapper that calls this function; the host
/// calls it once, right after loading the library.
///
/// # Safety
///
/// `cell` must point to the host's deadline cell from the matching lockstep
/// build; it lives for the process.
pub unsafe fn share_deadline_v1(cell: *const libc::c_void) {
    // SAFETY: the checked host passes its 'static cell of the same layout.
    crate::deadline::adopt(unsafe { &*cell.cast::<crate::deadline::ProcessDeadline>() });
}

/// Digest of the lockstep Rust ABI required by first-party bundle libraries.
///
/// Bundle libraries call this from their own linked copy of `hya-tool` before
/// the host transfers a `Vec<Arc<dyn Tool>>` or `Vec<Arc<dyn Rewriter>>`
/// through a registration symbol. It covers the compiler and every source
/// file whose types cross the boundary.
#[must_use]
pub fn abi_digest_v1() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"hya.tool.native-bundle-abi/v1");
    hash.update(hya_version::BACKEND_VERSION.as_bytes());
    hash.update(hya_version::RUSTC.as_bytes());
    hash.update(std::env::consts::OS.as_bytes());
    hash.update(std::env::consts::ARCH.as_bytes());
    hash.update(std::mem::size_of::<ToolCtx>().to_be_bytes());
    hash.update(std::mem::align_of::<ToolCtx>().to_be_bytes());
    hash.update(std::mem::size_of::<ToolError>().to_be_bytes());
    hash.update(std::mem::size_of::<Arc<dyn Tool>>().to_be_bytes());
    hash.update(std::mem::size_of::<Arc<dyn Rewriter>>().to_be_bytes());
    hash.update(std::mem::size_of::<tokio::runtime::Handle>().to_be_bytes());
    hash.update(std::mem::align_of::<tokio::runtime::Handle>().to_be_bytes());
    hash.update(include_bytes!("tool.rs"));
    hash.update(include_bytes!("deadline.rs"));
    hash.update(include_bytes!("lifecycle.rs"));
    hash.update(include_bytes!("skill.rs"));
    hash.update(hya_tool_error::NATIVE_ABI_SOURCE);
    for source in hya_context::NATIVE_ABI_SOURCES {
        hash.update(source);
    }
    hash.finalize().into()
}

/// What one native bundle library registers with the host.
pub struct Registrations {
    /// Tools from `hya_tool_bundle_register_v1`, possibly none.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Rewriters from the optional `hya_tool_bundle_register_rewriters_v1`.
    pub rewriters: Vec<Arc<dyn Rewriter>>,
}

/// Return the process-lifetime deadline cell to pass to a checked native library.
#[doc(hidden)]
#[must_use]
pub fn deadline_cell_v1() -> *const std::ffi::c_void {
    std::ptr::from_ref(crate::deadline::cell()).cast()
}
