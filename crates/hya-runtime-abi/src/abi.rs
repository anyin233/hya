//! Lockstep Rust ABI v2 descriptor, digest, registration container, and export macro.

use std::fmt::Display;
use std::sync::Arc;

use hya_context::rewrite::Rewriter;
use hya_tool::{Tool, ToolCtx, ToolError};
use sha2::{Digest, Sha256};

use crate::extensions::{
    NativeBundleApiHandler, NativeDriver, NativeHook, NativeService, ProjectionReducer,
    UriSchemaHandler,
};
use crate::host::HostRuntime;
use crate::{AbiError, HostError};

/// Major version of the lockstep native runtime ABI.
pub const ABI_VERSION_V2: u32 = 2;

/// NUL-terminated C symbol name returning the 32-byte lockstep ABI v2 digest.
pub const SYMBOL_ABI_V2: &[u8] = b"hya_native_abi_v2\0";

/// NUL-terminated C symbol name entering the bundle's Tokio runtime to poll a future.
pub const SYMBOL_WITH_RUNTIME_V2: &[u8] = b"hya_native_with_runtime_v2\0";

/// NUL-terminated C symbol name adopting the host's process run-deadline cell.
pub const SYMBOL_SHARE_DEADLINE_V2: &[u8] = b"hya_native_share_deadline_v2\0";

/// NUL-terminated C symbol name invoking bundle registration with [`NativePluginContext`].
pub const SYMBOL_REGISTER_V2: &[u8] = b"hya_native_register_v2\0";

/// C function signature for [`SYMBOL_ABI_V2`].
pub type AbiDigestFn = unsafe extern "C" fn(*mut u8);

/// C function signature for [`SYMBOL_WITH_RUNTIME_V2`].
pub type WithRuntimeFn = unsafe extern "C" fn(
    *const libc::c_void,
    unsafe extern "C" fn(*mut libc::c_void),
    *mut libc::c_void,
);

/// C function signature for [`SYMBOL_SHARE_DEADLINE_V2`].
pub type ShareDeadlineFn = unsafe extern "C" fn(*const libc::c_void);

/// C function signature for [`SYMBOL_REGISTER_V2`].
pub type RegisterV2Fn = unsafe extern "C" fn(
    *const NativePluginContext,
    *mut NativeRegistrations,
    *mut Option<String>,
) -> i32;

/// Metadata and symbol descriptors for Native ABI v2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeAbiV2 {
    /// ABI version number (`2`).
    pub version: u32,
    /// Lockstep 32-byte digest for this host/bundle build.
    pub digest: [u8; 32],
}

impl NativeAbiV2 {
    /// Return the ABI v2 descriptor for the current build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            version: ABI_VERSION_V2,
            digest: abi_digest_v2(),
        }
    }
}

/// Host-supplied context handed to a native bundle during `hya_native_register_v2`.
#[derive(Clone)]
pub struct NativePluginContext {
    bundle_id: String,
    host_runtime: Arc<dyn HostRuntime>,
}

impl NativePluginContext {
    /// Construct a new registration context for `bundle_id`.
    #[must_use]
    pub fn new(bundle_id: impl Into<String>, host_runtime: Arc<dyn HostRuntime>) -> Self {
        Self {
            bundle_id: bundle_id.into(),
            host_runtime,
        }
    }

    /// Stable bundle identity being registered (e.g. `hya/goal`).
    #[must_use]
    pub fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    /// Host runtime handle providing access to all host capability planes.
    #[must_use]
    pub fn runtime(&self) -> &Arc<dyn HostRuntime> {
        &self.host_runtime
    }
}

/// Complete set of native extensions registered by one ABI v2 bundle.
#[derive(Default)]
pub struct NativeRegistrations {
    /// Model-callable tools.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Context rewriter extensions.
    pub rewriters: Vec<Arc<dyn Rewriter>>,
    /// Managed background services.
    pub services: Vec<Arc<dyn NativeService>>,
    /// Runtime lifecycle and execution hooks.
    pub hooks: Vec<Arc<dyn NativeHook>>,
    /// Bundle HTTP/gRPC API request handlers.
    pub api_handlers: Vec<Arc<dyn NativeBundleApiHandler>>,
    /// Event-log projection reducers for `Event::Extension`.
    pub projection_reducers: Vec<Arc<dyn ProjectionReducer>>,
    /// Custom URI scheme handlers for `HandleRouter`.
    pub uri_schema_handlers: Vec<Arc<dyn UriSchemaHandler>>,
    /// Hook points this bundle declares, as local names; the host namespaces
    /// them as `<bundle-id>:<name>` so other bundles can implement them.
    pub hook_points: Vec<String>,
    /// Drivers that own a session run (namespaced `<bundle-id>:<name>`).
    pub drivers: Vec<Arc<dyn NativeDriver>>,
}

impl NativeRegistrations {
    /// Create an empty registration container.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return `true` if no registrations of any kind have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
            && self.rewriters.is_empty()
            && self.services.is_empty()
            && self.hooks.is_empty()
            && self.api_handlers.is_empty()
            && self.projection_reducers.is_empty()
            && self.uri_schema_handlers.is_empty()
            && self.hook_points.is_empty()
            && self.drivers.is_empty()
    }

    /// Merge all registrations from `other` into `self`, preserving order.
    pub fn extend(&mut self, other: Self) {
        self.tools.extend(other.tools);
        self.rewriters.extend(other.rewriters);
        self.services.extend(other.services);
        self.hooks.extend(other.hooks);
        self.api_handlers.extend(other.api_handlers);
        self.projection_reducers.extend(other.projection_reducers);
        self.uri_schema_handlers.extend(other.uri_schema_handlers);
        self.hook_points.extend(other.hook_points);
        self.drivers.extend(other.drivers);
    }
}

/// Helper trait allowing `export_native_bundle_v2!` blocks to return either
/// `()` or `Result<(), E>`.
pub trait IntoRegistrationResult {
    /// Convert the registration block return value into a diagnostic `Result`.
    fn into_registration_result(self) -> Result<(), String>;
}

impl IntoRegistrationResult for () {
    fn into_registration_result(self) -> Result<(), String> {
        Ok(())
    }
}

impl<E: Display> IntoRegistrationResult for Result<(), E> {
    fn into_registration_result(self) -> Result<(), String> {
        self.map_err(|error| error.to_string())
    }
}

/// Compute the lockstep ABI v2 digest verified before any Rust object crosses
/// the host/bundle FFI boundary.
///
/// The digest covers:
/// - the ABI v2 domain separator,
/// - `hya_version::VERSION`, `hya_version::BACKEND_VERSION`, and `hya_version::RUSTC`,
/// - `std::env::consts::OS` and `std::env::consts::ARCH`,
/// - `hya_tool::native_bundle::abi_digest_v1()`,
/// - sizes and alignments of every ABI-crossing context, registration, and trait object type,
/// - raw source bytes of every `hya-runtime-abi`, `hya-tool-error`, and `hya-context` ABI file.
#[must_use]
pub fn abi_digest_v2() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"hya.runtime.native-bundle-abi/v2");
    hash.update(ABI_VERSION_V2.to_be_bytes());
    hash.update(hya_version::VERSION.as_bytes());
    hash.update(hya_version::BACKEND_VERSION.as_bytes());
    hash.update(hya_version::RUSTC.as_bytes());
    hash.update(std::env::consts::OS.as_bytes());
    hash.update(std::env::consts::ARCH.as_bytes());
    hash.update(hya_tool::native_bundle::abi_digest_v1());

    record_layout::<NativePluginContext>(&mut hash);
    record_layout::<NativeRegistrations>(&mut hash);
    record_layout::<HostError>(&mut hash);
    record_layout::<AbiError>(&mut hash);
    record_layout::<Arc<dyn HostRuntime>>(&mut hash);
    record_layout::<Arc<dyn Tool>>(&mut hash);
    record_layout::<Arc<dyn Rewriter>>(&mut hash);
    record_layout::<Arc<dyn NativeService>>(&mut hash);
    record_layout::<Arc<dyn NativeHook>>(&mut hash);
    record_layout::<Arc<dyn NativeBundleApiHandler>>(&mut hash);
    record_layout::<Arc<dyn ProjectionReducer>>(&mut hash);
    record_layout::<Arc<dyn UriSchemaHandler>>(&mut hash);
    record_layout::<Arc<dyn NativeDriver>>(&mut hash);
    record_layout::<ToolCtx>(&mut hash);
    record_layout::<ToolError>(&mut hash);
    record_layout::<tokio::runtime::Handle>(&mut hash);
    record_layout::<Option<String>>(&mut hash);

    hash.update(include_bytes!("lib.rs"));
    hash.update(include_bytes!("error.rs"));
    hash.update(include_bytes!("host.rs"));
    hash.update(include_bytes!("extensions.rs"));
    hash.update(include_bytes!("abi.rs"));
    hash.update(hya_tool_error::NATIVE_ABI_SOURCE);
    for source in hya_context::NATIVE_ABI_SOURCES {
        hash.update(source);
    }
    hash.finalize().into()
}

fn record_layout<T>(hash: &mut Sha256) {
    hash.update(std::mem::size_of::<T>().to_be_bytes());
    hash.update(std::mem::align_of::<T>().to_be_bytes());
}

/// Enter the bundle's Tokio runtime while synchronously polling one native future.
///
/// # Safety
///
/// `handle` must point to a live `tokio::runtime::Handle` from the matching
/// lockstep build, and `callback` must accept `state` without retaining it
/// after returning.
pub unsafe fn with_runtime_v2(
    handle: *const libc::c_void,
    callback: unsafe extern "C" fn(*mut libc::c_void),
    state: *mut libc::c_void,
) {
    // SAFETY: shares the exact lockstep Tokio bridge with `hya-tool`.
    unsafe { hya_tool::native_bundle::with_runtime_v1(handle, callback, state) };
}

/// Adopt the host's process-lifetime deadline cell inside this native bundle.
///
/// # Safety
///
/// `cell` must point to the host's process deadline cell from the matching
/// lockstep build.
pub unsafe fn share_deadline_v2(cell: *const libc::c_void) {
    // SAFETY: shares the exact lockstep deadline cell with `hya-tool`.
    unsafe { hya_tool::native_bundle::share_deadline_v1(cell) };
}

/// Return the host's process-lifetime deadline cell pointer for passing to
/// `hya_native_share_deadline_v2`.
#[doc(hidden)]
#[must_use]
pub fn deadline_cell_v2() -> *const std::ffi::c_void {
    hya_tool::native_bundle::deadline_cell_v1()
}

/// Export the four unified Lockstep Native ABI v2 symbols for a native bundle.
///
/// Forms supported:
/// - `export_native_bundle_v2!(|ctx, out| { ... });` (block evaluates to `()` or `Result<(), E>`)
/// - `export_native_bundle_v2!(|out| { ... });` (block evaluates to `()` or `Result<(), E>`)
#[macro_export]
macro_rules! export_native_bundle_v2 {
    (|$ctx:ident, $out:ident| $register:block) => {
        /// Report the lockstep ABI v2 digest before any Rust object crosses the FFI boundary.
        ///
        /// # Safety
        ///
        /// `out` must point to a writable array of at least 32 bytes, or be null.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hya_native_abi_v2(out: *mut u8) {
            if out.is_null() {
                return;
            }
            let digest = $crate::abi_digest_v2();
            // SAFETY: caller guarantees `out` points to 32 writable bytes.
            unsafe { ::std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len()) };
        }

        /// Poll one native future while this library's Tokio runtime TLS is entered.
        ///
        /// # Safety
        ///
        /// `handle` and `state` must remain valid for the duration of `callback`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hya_native_with_runtime_v2(
            handle: *const ::std::ffi::c_void,
            callback: unsafe extern "C" fn(*mut ::std::ffi::c_void),
            state: *mut ::std::ffi::c_void,
        ) {
            // SAFETY: host checked ABI v2 digest prior to invoking this export.
            unsafe { $crate::with_runtime_v2(handle, callback, state) };
        }

        /// Adopt the host's run-deadline cell inside this library's `hya-tool` copy.
        ///
        /// # Safety
        ///
        /// `cell` must point to the host's process-lifetime deadline cell.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hya_native_share_deadline_v2(cell: *const ::std::ffi::c_void) {
            // SAFETY: host checked ABI v2 digest prior to invoking this export.
            unsafe { $crate::share_deadline_v2(cell) };
        }

        /// Register this bundle's native extensions into `out` using `ctx`.
        ///
        /// # Safety
        ///
        /// `ctx`, `out`, and `error_out` (if non-null) must be valid pointers
        /// created by a host whose ABI v2 digest matched `hya_native_abi_v2`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hya_native_register_v2(
            ctx: *const $crate::NativePluginContext,
            out: *mut $crate::NativeRegistrations,
            error_out: *mut ::std::option::Option<::std::string::String>,
        ) -> i32 {
            // SAFETY: host verified ABI v2 digest before invoking registration.
            let (Some($ctx), Some($out)) = (unsafe { ctx.as_ref() }, unsafe { out.as_mut() })
            else {
                if let Some(err_slot) = unsafe { error_out.as_mut() } {
                    *err_slot = ::std::option::Option::Some(
                        "null NativePluginContext or NativeRegistrations pointer".to_string(),
                    );
                }
                return -1;
            };
            let result = (|| $register)();
            match $crate::IntoRegistrationResult::into_registration_result(result) {
                ::std::result::Result::Ok(()) => 0,
                ::std::result::Result::Err(message) => {
                    // SAFETY: `error_out` is either null or a live `Option<String>`.
                    if let Some(err_slot) = unsafe { error_out.as_mut() } {
                        *err_slot = ::std::option::Option::Some(message);
                    }
                    1
                }
            }
        }
    };
    (|$out:ident| $register:block) => {
        $crate::export_native_bundle_v2!(|_ctx, $out| $register);
    };
}
