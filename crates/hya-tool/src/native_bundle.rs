//! Lockstep dynamic-library loader for first-party tool bundles.
//!
//! Rust trait objects do not have a stable cross-compiler ABI. These libraries
//! are built from the same workspace and toolchain as the host. The loader
//! checks an ABI digest before passing any Rust-owned object across the FFI
//! boundary, then keeps the library mapped for the process lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use hya_bundle::inspect_public_package;
use sha2::{Digest, Sha256};

use crate::{Tool, ToolCtx, ToolError};

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
    let mut tools = Vec::new();
    // SAFETY: `tools` is a live Vec with the exact type expected by the
    // matching, lockstep bundle library.
    unsafe { register(&mut tools) };
    Ok(tools)
}

#[cfg(unix)]
fn open_library(stem: &str) -> Result<usize, String> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;

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
    let package = parent
        .parent()
        .unwrap_or(parent)
        .join("bundles")
        .join(format!("{}.hyabundle", stem.replace('_', "-")));
    let library = if package.is_file() {
        extract_packaged_library(&package, stem, &filename)?
    } else {
        let candidates = [parent.join(&filename), parent.join("deps").join(&filename)];
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                format!(
                    "native tool bundle `{}` is missing near {executable:?}",
                    package.display()
                )
            })?
    };
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
    package: &std::path::Path,
    stem: &str,
    filename: &str,
) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::DirBuilderExt as _;
    let bytes = std::fs::read(package).map_err(|error| error.to_string())?;
    let catalog = inspect_public_package(&bytes).map_err(|error| error.to_string())?;
    let [bundle] = catalog.bundles() else {
        return Err(format!(
            "{} must contain exactly one bundle",
            package.display()
        ));
    };
    let identity = format!(
        "hya/{}",
        stem.strip_prefix("hya_").unwrap_or(stem).replace('_', "-")
    );
    if bundle.identity().id != identity || !catalog.process_extensions().is_empty() {
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
