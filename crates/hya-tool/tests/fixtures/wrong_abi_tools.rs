//! A library with an incompatible ABI fingerprint; registration must never run.
/// # Safety
/// `out` points to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hya_tool_bundle_abi_v1(out: *mut u8) {
    unsafe { std::ptr::write_bytes(out, 0, 32) };
}
/// Abort if the host registers an incompatible library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_tool_bundle_register_v1() {
    std::process::abort();
}
/// Abort if the host polls an incompatible library.
#[unsafe(no_mangle)]
pub extern "C" fn hya_tool_bundle_with_runtime_v1() {
    std::process::abort();
}
