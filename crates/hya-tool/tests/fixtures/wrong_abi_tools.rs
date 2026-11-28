//! A library with an incompatible ABI v2 fingerprint; registration must never run.

/// # Safety
/// `out` points to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_native_abi_v2(out: *mut u8) {
    if !out.is_null() {
        unsafe { std::ptr::write_bytes(out, 0, 32) };
    }
}

/// Abort if the host registers an incompatible library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_register_v2() -> i32 {
    std::process::abort();
}

/// Abort if the host polls an incompatible library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_with_runtime_v2() {
    std::process::abort();
}

/// Abort if the host shares state with an incompatible library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_native_share_deadline_v2() {
    std::process::abort();
}
