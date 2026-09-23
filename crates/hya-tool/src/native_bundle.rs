//! Lockstep dynamic-library loader for first-party tool bundles.
//!
//! Rust trait objects do not have a stable cross-compiler ABI. These libraries
//! are built from the same workspace and toolchain as the host. The loader
//! checks an ABI digest before passing any Rust-owned object across the FFI
//! boundary, then keeps the library mapped for the process lifetime.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::future::{AbortHandle, Abortable};
use hya_bundle::{FirstPartySource, first_party_bundle, first_party_source, load_first_party};
use hya_proto::ToolSchema;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolCtx, ToolError, ToolResultPolicy};

type RuntimeEntry = unsafe extern "C" fn(
    *const libc::c_void,
    unsafe extern "C" fn(*mut libc::c_void),
    *mut libc::c_void,
);

type NativeFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, ToolError>> + Send + 'a>>;

struct NativePollState {
    // The callback runs synchronously before `execute` releases these values.
    future: *mut NativeFuture<'static>,
    context: *mut Context<'static>,
    result: Option<Poll<Result<Value, ToolError>>>,
}

unsafe extern "C" fn poll_native_future(state: *mut libc::c_void) {
    // SAFETY: `with_runtime_v1` invokes this callback synchronously while the
    // caller's future, context, and state are live. Their erased lifetimes are
    // never retained beyond this call.
    let state = unsafe { &mut *state.cast::<NativePollState>() };
    // SAFETY: the synchronous callback owns exclusive access for this poll.
    let future = unsafe { &mut *state.future };
    let context = unsafe { &mut *state.context };
    state.result = Some(future.as_mut().poll(context));
}

struct NativeTool {
    inner: Arc<dyn Tool>,
    with_runtime: RuntimeEntry,
}

struct NativeWorkerGuard {
    abort: AbortHandle,
    cancel: CancellationToken,
    completed: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for NativeWorkerGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel.cancel();
        }
        self.abort.abort();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[async_trait]
impl Tool for NativeTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                poll_with_runtime(self.inner.execute(ctx, input), self.with_runtime, &handle).await
            }
            Err(_) => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        ToolError::Other(format!("start native tool runtime: {error}"))
                    })?;
                let handle = runtime.handle().clone();
                runtime.block_on(execute_with_handle(
                    Arc::clone(&self.inner),
                    self.with_runtime,
                    ctx.clone(),
                    input,
                    handle,
                ))
            }
        }
    }
}

async fn execute_with_handle(
    inner: Arc<dyn Tool>,
    with_runtime: RuntimeEntry,
    ctx: ToolCtx,
    input: Value,
    handle: tokio::runtime::Handle,
) -> Result<Value, ToolError> {
    let (abort, registration) = AbortHandle::new_pair();
    let cancel = ctx.cancel.clone();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("hya-native-tool".to_string())
        .spawn(move || {
            let _host_guard = handle.enter();
            let result = futures::executor::block_on(Abortable::new(
                poll_with_runtime(inner.execute(&ctx, input), with_runtime, &handle),
                registration,
            ));
            let result = result.unwrap_or(Err(ToolError::Cancelled));
            let _ = sender.send(result);
        })?;
    let mut guard = NativeWorkerGuard {
        abort,
        cancel,
        completed: false,
        thread: Some(thread),
    };
    let result = receiver
        .await
        .map_err(|error| ToolError::Other(format!("native tool worker failed: {error}")));
    guard.completed = true;
    drop(guard);
    result?
}

async fn poll_with_runtime(
    mut future: NativeFuture<'_>,
    with_runtime: RuntimeEntry,
    handle: &tokio::runtime::Handle,
) -> Result<Value, ToolError> {
    std::future::poll_fn(|context| {
        let mut state = NativePollState {
            future: (&mut future as *mut NativeFuture<'_>).cast::<NativeFuture<'static>>(),
            context: (context as *mut Context<'_>).cast::<Context<'static>>(),
            result: None,
        };
        // SAFETY: `with_runtime` belongs to the checked lockstep library and
        // polls synchronously before these locals drop.
        unsafe {
            (with_runtime)(
                (handle as *const tokio::runtime::Handle).cast(),
                poll_native_future,
                (&mut state as *mut NativePollState).cast(),
            );
        }
        state.result.unwrap_or_else(|| {
            Poll::Ready(Err(ToolError::Other(
                "native tool runtime bridge did not poll the future".to_string(),
            )))
        })
    })
    .await
}

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

/// Digest of the lockstep Rust tool ABI required by built-in bundle libraries.
///
/// Bundle libraries call this from their own linked copy of `hya-tool` before
/// the host transfers a `Vec<Arc<dyn Tool>>` through the registration symbol.
#[must_use]
pub fn abi_digest_v1() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"hya.tool.native-bundle-abi/v1");
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update(std::env::consts::OS.as_bytes());
    hash.update(std::env::consts::ARCH.as_bytes());
    hash.update(std::mem::size_of::<ToolCtx>().to_be_bytes());
    hash.update(std::mem::align_of::<ToolCtx>().to_be_bytes());
    hash.update(std::mem::size_of::<ToolError>().to_be_bytes());
    hash.update(std::mem::size_of::<Arc<dyn Tool>>().to_be_bytes());
    hash.update(std::mem::size_of::<tokio::runtime::Handle>().to_be_bytes());
    hash.update(std::mem::align_of::<tokio::runtime::Handle>().to_be_bytes());
    hash.update(include_bytes!("tool.rs"));
    hash.finalize().into()
}

static LIBRARIES: OnceLock<Mutex<HashMap<&'static str, usize>>> = OnceLock::new();

pub(crate) fn load_family(stem: &'static str) -> Result<Vec<Arc<dyn Tool>>, String> {
    let libraries = LIBRARIES.get_or_init(|| Mutex::new(HashMap::new()));
    let handle = {
        let mut libraries = libraries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match libraries.get(stem) {
            Some(handle) => *handle,
            None => {
                let loaded = open_library(stem)?;
                libraries.insert(stem, loaded);
                loaded
            }
        }
    };
    let register = symbol(handle, b"hya_tool_bundle_register_v1\0")?;
    // SAFETY: the symbol is exported by a library whose ABI digest matched
    // this host. The library remains mapped for the process lifetime.
    let register: unsafe extern "C" fn(*mut Vec<Arc<dyn Tool>>) =
        unsafe { std::mem::transmute(register) };
    let with_runtime = symbol(handle, b"hya_tool_bundle_with_runtime_v1\0")?;
    // SAFETY: this checked bundle symbol has the documented C callback ABI.
    let with_runtime: RuntimeEntry = unsafe { std::mem::transmute(with_runtime) };
    let mut tools = Vec::new();
    // SAFETY: `tools` is a live Vec with the exact type expected by the
    // matching, lockstep bundle library.
    unsafe { register(&mut tools) };
    Ok(tools
        .into_iter()
        .map(|inner| {
            Arc::new(NativeTool {
                inner,
                with_runtime,
            }) as Arc<dyn Tool>
        })
        .collect())
}

#[cfg(unix)]
fn native_package_directory(parent: &std::path::Path) -> std::path::PathBuf {
    let root = match parent.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("bin" | "deps") => parent.parent().unwrap_or(parent),
        _ => parent,
    };
    root.join("bundles")
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum LibrarySource {
    Local(std::path::PathBuf),
    Package(std::path::PathBuf),
}

/// Choose the lockstep library to load for one family.
///
/// A Cargo build places the library it just linked in `deps/` (and may uplift
/// a copy beside the executable). That library always matches the running
/// host, while a package staged under `bundles/` can be stale and is costly to
/// verify in unoptimized builds. Installed layouts carry no adjacent library,
/// so they load the verified package beside `bin/`.
#[cfg(unix)]
fn library_source(parent: &std::path::Path, filename: &str, stem: &str) -> Option<LibrarySource> {
    let candidates = [parent.join("deps").join(filename), parent.join(filename)];
    if let Some(local) = candidates.into_iter().find(|path| path.is_file()) {
        return Some(LibrarySource::Local(local));
    }
    let package =
        native_package_directory(parent).join(format!("{}.hyabundle", stem.replace('_', "-")));
    package.is_file().then_some(LibrarySource::Package(package))
}

#[cfg(unix)]
fn open_library(stem: &str) -> Result<usize, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = executable
        .parent()
        .ok_or_else(|| "backend executable has no parent directory".to_string())?;
    let filename = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        stem,
        std::env::consts::DLL_SUFFIX
    );
    match library_source(parent, &filename, stem) {
        Some(LibrarySource::Local(path)) => load_checked(stem, &path),
        Some(LibrarySource::Package(package)) => {
            let extracted = extract_packaged_library(parent, &package, stem, &filename)?;
            load_extracted(stem, &extracted)
        }
        None => Err(format!(
            "native tool bundle `{stem}` is missing near {executable:?}"
        )),
    }
}

/// Load a library extracted from its package, then delete the extracted copy.
///
/// The loaded image stays mapped after its file is unlinked, so nothing is
/// left in the temporary directory once the process has the library.
#[cfg(unix)]
fn load_extracted(stem: &str, extracted: &std::path::Path) -> Result<usize, String> {
    let loaded = load_checked(stem, extracted);
    let _ = std::fs::remove_file(extracted);
    if let Some(directory) = extracted.parent().filter(|directory| {
        directory
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with("hya-native-tool-"))
    }) {
        let _ = std::fs::remove_dir(directory);
    }
    loaded
}

/// `dlopen` one lockstep library and verify its ABI digest.
#[cfg(unix)]
fn load_checked(stem: &str, library: &std::path::Path) -> Result<usize, String> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(library.as_os_str().as_bytes())
        .map_err(|_| "native tool bundle path contains a NUL byte".to_string())?;
    // SAFETY: `path` is NUL terminated; RTLD_NOW resolves all symbols before
    // any bundle registration code executes.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: dlerror returns a process-owned NUL-terminated diagnostic.
        let detail = unsafe { libc::dlerror() };
        let detail = if detail.is_null() {
            "unknown loader failure".to_string()
        } else {
            // SAFETY: non-null `dlerror` points to its NUL-terminated message.
            unsafe { CStr::from_ptr(detail) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(format!("load native tool bundle {stem}: {detail}"));
    }
    let handle = handle as usize;
    let abi = symbol(handle, b"hya_tool_bundle_abi_v1\0")?;
    // SAFETY: this symbol has a C ABI and writes exactly 32 bytes to `out`.
    let abi: unsafe extern "C" fn(*mut u8) = unsafe { std::mem::transmute(abi) };
    let mut actual = [0_u8; 32];
    // SAFETY: `actual` is a writable 32-byte array.
    unsafe { abi(actual.as_mut_ptr()) };
    if actual != abi_digest_v1() {
        return Err(format!(
            "native tool bundle `{stem}` was built for a different hya-tool ABI"
        ));
    }
    Ok(handle)
}

#[cfg(unix)]
fn extract_packaged_library(
    executable_dir: &std::path::Path,
    package: &std::path::Path,
    stem: &str,
    filename: &str,
) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let identity = format!(
        "hya/{}",
        stem.strip_prefix("hya_").unwrap_or(stem).replace('_', "-")
    );
    // Reuse the process catalog when the first-party loader resolved this same
    // package (installed layout); otherwise verify the staged package directly.
    let staged;
    let catalog = match first_party_source(executable_dir, &identity) {
        Some(FirstPartySource::Package(resolved)) if resolved == package => {
            first_party_bundle(&identity).map_err(|error| error.to_string())?
        }
        _ => {
            staged = load_first_party(&FirstPartySource::Package(package.to_path_buf()), &identity)
                .map_err(|error| error.to_string())?;
            &staged
        }
    };
    let [bundle] = catalog.bundles() else {
        return Err(format!(
            "{} must contain exactly one bundle",
            package.display()
        ));
    };
    if !catalog.process_extensions().is_empty() {
        return Err(format!(
            "{} has an unexpected native tool identity or process",
            package.display()
        ));
    }
    let libraries = bundle
        .extensions()
        .iter()
        .filter(|resource| resource.binary_base64.is_some())
        .collect::<Vec<_>>();
    let [library] = libraries.as_slice() else {
        return Err(format!(
            "{} must contain exactly one native library",
            package.display()
        ));
    };
    if library.local_id != "runtime" || !library.stable_id.contains("/library/") {
        return Err(format!(
            "{} has an unexpected native library",
            package.display()
        ));
    }
    let expected = crate::base_tools::tool_bundle_presets()
        .iter()
        .find(|preset| preset.identity() == identity)
        .ok_or_else(|| format!("unknown native tool family {identity}"))?;
    let declared = bundle
        .tools()
        .iter()
        .map(|tool| tool.local_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let exposed = expected
        .tools()
        .iter()
        .map(|tool| tool.name())
        .collect::<std::collections::BTreeSet<_>>();
    if declared != exposed {
        return Err(format!(
            "{} declares a different tool set",
            package.display()
        ));
    }
    let payload = library.source_bytes().map_err(|error| error.to_string())?;
    let digest = Sha256::digest(&payload);
    let prefix = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let directory =
        std::env::temp_dir().join(format!("hya-native-tool-{}-{prefix}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|error| format!("create private native tool directory: {error}"))?;
    let path = directory.join(filename);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(&payload)
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path).map_err(|error| error.to_string())? != payload {
                return Err(format!(
                    "native tool library cache collision at {}",
                    path.display()
                ));
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(path)
}

#[cfg(not(unix))]
fn open_library(stem: &str) -> Result<usize, String> {
    Err(format!(
        "native tool bundle `{stem}` requires a Unix dynamic loader"
    ))
}

#[cfg(unix)]
fn symbol(handle: usize, name: &[u8]) -> Result<usize, String> {
    // SAFETY: `handle` is retained in `LIBRARIES`, and `name` is a static,
    // NUL-terminated symbol name supplied by this module.
    let pointer = unsafe { libc::dlsym(handle as *mut libc::c_void, name.as_ptr().cast()) };
    if pointer.is_null() {
        return Err(format!(
            "native tool bundle missing symbol {}",
            String::from_utf8_lossy(name)
        ));
    }
    Ok(pointer as usize)
}

#[cfg(not(unix))]
fn symbol(_handle: usize, _name: &[u8]) -> Result<usize, String> {
    Err("native tool bundle symbol lookup requires Unix".to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    #[test]
    fn native_package_directory_matches_backend_and_test_layouts() {
        assert_eq!(
            super::native_package_directory(Path::new("/tmp/target/debug")),
            Path::new("/tmp/target/debug/bundles")
        );
        assert_eq!(
            super::native_package_directory(Path::new("/tmp/target/debug/deps")),
            Path::new("/tmp/target/debug/bundles")
        );
        assert_eq!(
            super::native_package_directory(Path::new("/tmp/release/bin")),
            Path::new("/tmp/release/bundles")
        );
    }

    #[test]
    fn cargo_build_layout_prefers_fresh_library_over_staged_package() -> std::io::Result<()> {
        let root =
            std::env::temp_dir().join(format!("hya-native-library-select-{}", std::process::id()));
        let debug = root.join("target/debug");
        let bundles = debug.join("bundles");
        std::fs::create_dir_all(debug.join("deps"))?;
        std::fs::create_dir_all(&bundles)?;
        let fresh = debug.join("deps/libhya_demo_tools.so");
        std::fs::write(&fresh, b"fresh")?;
        std::fs::write(debug.join("libhya_demo_tools.so"), b"uplifted")?;
        std::fs::write(bundles.join("hya-demo-tools.hyabundle"), b"staged")?;

        for parent in [debug.clone(), debug.join("deps")] {
            assert_eq!(
                super::library_source(&parent, "libhya_demo_tools.so", "hya_demo_tools"),
                Some(super::LibrarySource::Local(fresh.clone())),
                "{}",
                parent.display()
            );
        }

        let installed = root.join("release");
        std::fs::create_dir_all(installed.join("bin"))?;
        std::fs::create_dir_all(installed.join("bundles"))?;
        let package = installed.join("bundles/hya-demo-tools.hyabundle");
        std::fs::write(&package, b"package")?;
        assert_eq!(
            super::library_source(
                &installed.join("bin"),
                "libhya_demo_tools.so",
                "hya_demo_tools"
            ),
            Some(super::LibrarySource::Package(package))
        );
        assert_eq!(
            super::library_source(&root, "libhya_demo_tools.so", "hya_demo_tools"),
            None
        );
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn extracted_library_copy_is_removed_after_loading() -> Result<(), String> {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let deps = executable.parent().ok_or("test binary has no parent")?;
        let filename = format!(
            "{}hya_todo_tools{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        );
        let directory = std::env::temp_dir().join(format!(
            "hya-native-tool-{}-removal-test",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let extracted = directory.join(&filename);
        std::fs::copy(deps.join(&filename), &extracted).map_err(|error| error.to_string())?;

        let handle = super::load_extracted("hya_todo_tools", &extracted)?;
        super::symbol(handle, b"hya_tool_bundle_register_v1\0")?;
        assert!(
            !extracted.exists(),
            "extracted library must not outlive loading"
        );
        assert!(
            !directory.exists(),
            "private extraction directory must be removed"
        );
        Ok(())
    }

    #[test]
    fn native_library_exposes_runtime_entry_for_async_tools() -> Result<(), String> {
        let handle = super::open_library("hya_extended_tools")?;
        super::symbol(handle, b"hya_tool_bundle_with_runtime_v1\0")?;
        Ok(())
    }
}
