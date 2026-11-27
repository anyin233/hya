//! Test-only cdylib with a mismatched ABI v2 digest; no registration symbol may run.

/// Report an incompatible 32-byte digest.
///
/// # Safety
/// `out` must point to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_native_abi_v2(out: *mut u8) {
    if !out.is_null() {
        // SAFETY: caller guarantees 32 writable bytes.
        unsafe { std::ptr::write_bytes(out, 0xDE, 32) };
    }
}

/// Abort if the host invokes runtime bridge on a digest-mismatched library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_with_runtime_v2() {
    std::process::abort();
}

/// Abort if the host shares deadline cell with a digest-mismatched library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_share_deadline_v2() {
    std::process::abort();
}

/// Abort if the host invokes registration on a digest-mismatched library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_register_v2() -> i32 {
    std::process::abort();
}
