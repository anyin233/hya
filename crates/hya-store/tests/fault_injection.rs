//! Fault-injection integration tests for [`hya_store`].
//!
//! This file is compiled as its own integration test binary separate from
//! `tests/it` because:
//! 1. It registers a custom SQLite VFS as the process-global default
//!    (`sqlite3_vfs_register(..., 1)`), since `SessionStore::connect` configures
//!    its own connection options.
//! 2. On Linux it interposes `clock_gettime` process-wide so pool lifetime and
//!    idle-timeout tests do not need to sleep 30+ minutes.
//!
//! Neither global hook may leak into `tests/it`.
//!
//! # Monotonic clock shim contract
//!
//! On Linux, [`clock_gettime`] adds a monotonically non-decreasing offset to
//! `CLOCK_MONOTONIC` and `CLOCK_MONOTONIC_RAW` for every caller in this binary
//! (bypassing vDSO for clock reads). Because the test harness (`libtest`) reads
//! the same monotonic clock, its printed summary reflects the simulated jump
//! (for example `finished in 1830.16s` and a `has been running for over 60
//! seconds` warning) even though actual wall-clock runtime is sub-second.
//!
//! **Every test in this file MUST hold [`clock_gate()`]** — `.write().await`
//! when advancing the clock, or `.read().await` otherwise — so no test advances
//! time concurrently under another test.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::ptr;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock, RwLock};
use std::time::Duration;

use hya_proto::{Event, SessionId};
use hya_store::{BundleRegistry, SessionStore};
use libsqlite3_sys::{
    SQLITE_BUSY, SQLITE_ERROR, SQLITE_IOERR_SHMLOCK, SQLITE_IOERR_SHMOPEN, SQLITE_LOCK_EXCLUSIVE,
    SQLITE_OK, SQLITE_SHM_EXCLUSIVE, SQLITE_SHM_LOCK, SQLITE_SHM_UNLOCK, sqlite3_file,
    sqlite3_filename, sqlite3_int64, sqlite3_io_methods, sqlite3_syscall_ptr, sqlite3_vfs,
    sqlite3_vfs_find, sqlite3_vfs_register,
};
use sqlx::Error as SqlxError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

type ReadHook = Box<dyn FnOnce() + Send>;

const SQLITE_SHM_LOCK_EXCL: c_int = SQLITE_SHM_LOCK | SQLITE_SHM_EXCLUSIVE;

#[cfg(target_os = "linux")]
static CLOCK_OFFSET_SECS: AtomicI64 = AtomicI64::new(0);

static CLOCK_TEST_GATE: OnceLock<tokio::sync::RwLock<()>> = OnceLock::new();

fn clock_gate() -> &'static tokio::sync::RwLock<()> {
    CLOCK_TEST_GATE.get_or_init(|| tokio::sync::RwLock::new(()))
}

/// Interpose Linux monotonic clock reads so connection-lifetime tests do not
/// need to sleep 30 minutes.
///
/// # Safety
///
/// Matches the libc `clock_gettime(2)` ABI: `tp` must be either null or a valid
/// writable pointer to `libc::timespec`.
#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clk_id: libc::clockid_t, tp: *mut libc::timespec) -> c_int {
    let rc = unsafe { libc::syscall(libc::SYS_clock_gettime, clk_id, tp) as c_int };
    if rc == 0
        && !tp.is_null()
        && (clk_id == libc::CLOCK_MONOTONIC || clk_id == libc::CLOCK_MONOTONIC_RAW)
    {
        unsafe {
            (*tp).tv_sec = (*tp)
                .tv_sec
                .saturating_add(CLOCK_OFFSET_SECS.load(Ordering::Relaxed));
        }
    }
    rc
}

#[cfg(target_os = "linux")]
fn advance_monotonic_secs(secs: i64) {
    assert!(secs >= 0, "monotonic clock offset must never decrease");
    CLOCK_OFFSET_SECS.fetch_add(secs, Ordering::SeqCst);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    MainDb,
    Wal,
    Other,
}

impl FileKind {
    fn from_path(path: &str) -> Self {
        if path.ends_with("-wal") {
            Self::Wal
        } else if path.ends_with(".db") || path.ends_with(".sqlite") || path.ends_with(".sqlite3") {
            Self::MainDb
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug)]
struct VfsEvent {
    kind: FileKind,
    op: &'static str,
    offset: c_int,
    arg: i64,
    rc: c_int,
}

struct FaultPolicy {
    wal_write_lock_holder: AtomicU64,
    shm_read_hook: Mutex<Option<(usize, ReadHook)>>,
    trace: Mutex<Vec<VfsEvent>>,
}

impl FaultPolicy {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            wal_write_lock_holder: AtomicU64::new(0),
            shm_read_hook: Mutex::new(None),
            trace: Mutex::new(Vec::new()),
        })
    }

    fn record(&self, kind: FileKind, op: &'static str, offset: c_int, arg: i64, rc: c_int) {
        let ev = VfsEvent {
            kind,
            op,
            offset,
            arg,
            rc,
        };
        self.trace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ev);
    }

    fn take_trace(&self) -> Vec<VfsEvent> {
        std::mem::take(
            &mut *self
                .trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn arm_shm_read_hook(&self, skip_reads: usize, hook: impl FnOnce() + Send + 'static) {
        *self
            .shm_read_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((skip_reads, Box::new(hook)));
    }

    fn clear_shm_read_hook(&self) -> bool {
        self.shm_read_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .is_some()
    }
}

fn current_thread_tag() -> u64 {
    static NEXT_TAG: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        static TAG: u64 = NEXT_TAG.fetch_add(1, Ordering::Relaxed);
    }
    TAG.with(|tag| *tag)
}

static REGISTRY: std::sync::LazyLock<RwLock<HashMap<PathBuf, Arc<FaultPolicy>>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

fn lookup_policy(path: &str) -> Option<Arc<FaultPolicy>> {
    let p = Path::new(path);
    let guard = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (prefix, pol) in guard.iter() {
        if p.starts_with(prefix) {
            return Some(Arc::clone(pol));
        }
    }
    None
}

static INSTALL_ONCE: Once = Once::new();
static mut PARENT_VFS: *mut sqlite3_vfs = ptr::null_mut();
static mut FAULT_VFS: std::mem::MaybeUninit<sqlite3_vfs> = std::mem::MaybeUninit::uninit();
static mut FAULT_VFS_NAME: *mut c_char = ptr::null_mut();

#[repr(C)]
struct FaultFile {
    base: sqlite3_file,
    inner: *mut sqlite3_file,
    state: *mut FaultFileState,
}

struct FaultFileState {
    kind: FileKind,
    policy: Option<Arc<FaultPolicy>>,
}

// VFS callbacks below execute across C FFI boundaries where panicking aborts
// the test binary (skipping Drop cleanup); hook closures and internal locks
// must therefore remain panic-free.
static FAULT_IO_METHODS: sqlite3_io_methods = sqlite3_io_methods {
    iVersion: 3,
    xClose: Some(vfs_x_close),
    xRead: Some(vfs_x_read),
    xWrite: Some(vfs_x_write),
    xTruncate: Some(vfs_x_truncate),
    xSync: Some(vfs_x_sync),
    xFileSize: Some(vfs_x_file_size),
    xLock: Some(vfs_x_lock),
    xUnlock: Some(vfs_x_unlock),
    xCheckReservedLock: Some(vfs_x_check_reserved_lock),
    xFileControl: Some(vfs_x_file_control),
    xSectorSize: Some(vfs_x_sector_size),
    xDeviceCharacteristics: Some(vfs_x_device_characteristics),
    xShmMap: Some(vfs_x_shm_map),
    xShmLock: Some(vfs_x_shm_lock),
    xShmBarrier: Some(vfs_x_shm_barrier),
    xShmUnmap: Some(vfs_x_shm_unmap),
    xFetch: Some(vfs_x_fetch),
    xUnfetch: Some(vfs_x_unfetch),
};

unsafe fn file_parts<'a>(p_file: *mut sqlite3_file) -> (&'a mut FaultFile, &'a sqlite3_io_methods) {
    unsafe {
        let ff = &mut *p_file.cast::<FaultFile>();
        let im = &*(*ff.inner).pMethods;
        (ff, im)
    }
}

unsafe extern "C" fn vfs_x_close(p_file: *mut sqlite3_file) -> c_int {
    unsafe {
        let ff = &mut *p_file.cast::<FaultFile>();
        let state = Box::from_raw(ff.state);
        ff.state = ptr::null_mut();
        let rc = if !ff.inner.is_null() && !(*ff.inner).pMethods.is_null() {
            let im = &*(*ff.inner).pMethods;
            im.xClose.map_or(SQLITE_OK, |f| f(ff.inner))
        } else {
            SQLITE_OK
        };
        if let Some(pol) = &state.policy {
            pol.record(state.kind, "xClose", 0, 0, rc);
        }
        if !ff.inner.is_null() {
            libc::free(ff.inner.cast());
            ff.inner = ptr::null_mut();
        }
        rc
    }
}

unsafe extern "C" fn vfs_x_read(
    p_file: *mut sqlite3_file,
    buf: *mut c_void,
    i_amt: c_int,
    i_ofst: sqlite3_int64,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        let state = &*ff.state;
        let rc = im.xRead.unwrap()(ff.inner, buf, i_amt, i_ofst);
        if let Some(pol) = &state.policy {
            pol.record(state.kind, "xRead", i_amt, i_ofst, rc);
        }
        if rc == SQLITE_OK
            && matches!(state.kind, FileKind::MainDb | FileKind::Wal)
            && let Some(pol) = &state.policy
        {
            let tid = current_thread_tag();
            if pol.wal_write_lock_holder.load(Ordering::Acquire) != tid {
                let mut guard = pol
                    .shm_read_hook
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let should_fire = if let Some((remaining_skips, _)) = guard.as_mut() {
                    if *remaining_skips > 0 {
                        *remaining_skips -= 1;
                        false
                    } else {
                        true
                    }
                } else {
                    false
                };
                if should_fire && let Some((_, hook)) = guard.take() {
                    drop(guard);
                    hook();
                }
            }
        }
        rc
    }
}

unsafe extern "C" fn vfs_x_write(
    p_file: *mut sqlite3_file,
    buf: *const c_void,
    i_amt: c_int,
    i_ofst: sqlite3_int64,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xWrite.unwrap()(ff.inner, buf, i_amt, i_ofst)
    }
}

unsafe extern "C" fn vfs_x_truncate(p_file: *mut sqlite3_file, size: sqlite3_int64) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xTruncate.unwrap()(ff.inner, size)
    }
}

unsafe extern "C" fn vfs_x_sync(p_file: *mut sqlite3_file, flags: c_int) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xSync.unwrap()(ff.inner, flags)
    }
}

unsafe extern "C" fn vfs_x_file_size(
    p_file: *mut sqlite3_file,
    p_size: *mut sqlite3_int64,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xFileSize.unwrap()(ff.inner, p_size)
    }
}

unsafe extern "C" fn vfs_x_lock(p_file: *mut sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        let state = &*ff.state;
        let rc = im.xLock.unwrap()(ff.inner, level);
        if let Some(pol) = &state.policy {
            pol.record(state.kind, "xLock", 0, i64::from(level), rc);
        }
        rc
    }
}

unsafe extern "C" fn vfs_x_unlock(p_file: *mut sqlite3_file, level: c_int) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xUnlock.unwrap()(ff.inner, level)
    }
}

unsafe extern "C" fn vfs_x_check_reserved_lock(
    p_file: *mut sqlite3_file,
    p_res_out: *mut c_int,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xCheckReservedLock.unwrap()(ff.inner, p_res_out)
    }
}

unsafe extern "C" fn vfs_x_file_control(
    p_file: *mut sqlite3_file,
    op: c_int,
    p_arg: *mut c_void,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xFileControl.unwrap()(ff.inner, op, p_arg)
    }
}

unsafe extern "C" fn vfs_x_sector_size(p_file: *mut sqlite3_file) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xSectorSize.map_or(4096, |f| f(ff.inner))
    }
}

unsafe extern "C" fn vfs_x_device_characteristics(p_file: *mut sqlite3_file) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xDeviceCharacteristics.map_or(0, |f| f(ff.inner))
    }
}

unsafe extern "C" fn vfs_x_shm_map(
    p_file: *mut sqlite3_file,
    i_pg: c_int,
    pgsz: c_int,
    is_write: c_int,
    pp: *mut *mut c_void,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        let Some(f) = im.xShmMap else {
            return SQLITE_IOERR_SHMOPEN;
        };
        f(ff.inner, i_pg, pgsz, is_write, pp)
    }
}

unsafe extern "C" fn vfs_x_shm_lock(
    p_file: *mut sqlite3_file,
    offset: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        let state = &*ff.state;
        let Some(f) = im.xShmLock else {
            return SQLITE_IOERR_SHMLOCK;
        };
        let rc = f(ff.inner, offset, n, flags);
        if let Some(pol) = &state.policy {
            pol.record(state.kind, "xShmLock", offset, i64::from(flags), rc);
            let tid = current_thread_tag();
            if offset == 0 && flags == SQLITE_SHM_LOCK_EXCL && rc == SQLITE_OK {
                pol.wal_write_lock_holder.store(tid, Ordering::Release);
            } else if offset == 0
                && (flags & SQLITE_SHM_UNLOCK) != 0
                && pol.wal_write_lock_holder.load(Ordering::Acquire) == tid
            {
                pol.wal_write_lock_holder.store(0, Ordering::Release);
            }
        }
        rc
    }
}

unsafe extern "C" fn vfs_x_shm_barrier(p_file: *mut sqlite3_file) {
    unsafe {
        let (ff, im) = file_parts(p_file);
        if let Some(f) = im.xShmBarrier {
            f(ff.inner);
        }
    }
}

unsafe extern "C" fn vfs_x_shm_unmap(p_file: *mut sqlite3_file, delete_flag: c_int) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xShmUnmap.map_or(SQLITE_OK, |f| f(ff.inner, delete_flag))
    }
}

unsafe extern "C" fn vfs_x_fetch(
    p_file: *mut sqlite3_file,
    i_ofst: sqlite3_int64,
    i_amt: c_int,
    pp: *mut *mut c_void,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        match im.xFetch {
            Some(f) => f(ff.inner, i_ofst, i_amt, pp),
            None => {
                *pp = ptr::null_mut();
                SQLITE_OK
            }
        }
    }
}

unsafe extern "C" fn vfs_x_unfetch(
    p_file: *mut sqlite3_file,
    i_ofst: sqlite3_int64,
    p: *mut c_void,
) -> c_int {
    unsafe {
        let (ff, im) = file_parts(p_file);
        im.xUnfetch.map_or(SQLITE_OK, |f| f(ff.inner, i_ofst, p))
    }
}

unsafe extern "C" fn vfs_x_open(
    _p_vfs: *mut sqlite3_vfs,
    z_name: sqlite3_filename,
    p_file: *mut sqlite3_file,
    flags: c_int,
    p_out_flags: *mut c_int,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        let path_str = if z_name.is_null() {
            String::new()
        } else {
            CStr::from_ptr(z_name).to_string_lossy().into_owned()
        };
        let policy = if path_str.is_empty() {
            None
        } else {
            lookup_policy(&path_str)
        };
        let kind = FileKind::from_path(&path_str);
        let inner = libc::calloc(1, parent.szOsFile as usize).cast::<sqlite3_file>();
        if inner.is_null() {
            return SQLITE_ERROR;
        }
        let rc = parent.xOpen.unwrap()(
            parent as *const _ as *mut _,
            z_name,
            inner,
            flags,
            p_out_flags,
        );
        if rc != SQLITE_OK {
            if !(*inner).pMethods.is_null()
                && let Some(close) = (*(*inner).pMethods).xClose
            {
                close(inner);
            }
            libc::free(inner.cast());
            (*p_file).pMethods = ptr::null();
            return rc;
        }
        let ff = &mut *p_file.cast::<FaultFile>();
        ff.base.pMethods = &raw const FAULT_IO_METHODS;
        ff.inner = inner;
        ff.state = Box::into_raw(Box::new(FaultFileState { kind, policy }));
        SQLITE_OK
    }
}

unsafe extern "C" fn vfs_x_delete(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
    sync_dir: c_int,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        let path_str = if z_name.is_null() {
            String::new()
        } else {
            CStr::from_ptr(z_name).to_string_lossy().into_owned()
        };
        let policy = lookup_policy(&path_str);
        let rc = parent.xDelete.unwrap()(parent as *const _ as *mut _, z_name, sync_dir);
        if let Some(pol) = &policy {
            pol.record(
                FileKind::from_path(&path_str),
                "xDelete",
                0,
                i64::from(sync_dir),
                rc,
            );
        }
        rc
    }
}

unsafe extern "C" fn vfs_x_access(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
    flags: c_int,
    p_res_out: *mut c_int,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xAccess.unwrap()(parent as *const _ as *mut _, z_name, flags, p_res_out)
    }
}

unsafe extern "C" fn vfs_x_full_pathname(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
    n_out: c_int,
    z_out: *mut c_char,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xFullPathname.unwrap()(parent as *const _ as *mut _, z_name, n_out, z_out)
    }
}

unsafe extern "C" fn vfs_x_dl_open(
    _p_vfs: *mut sqlite3_vfs,
    z_filename: *const c_char,
) -> *mut c_void {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xDlOpen.map_or(ptr::null_mut(), |f| {
            f(parent as *const _ as *mut _, z_filename)
        })
    }
}

unsafe extern "C" fn vfs_x_dl_error(_p_vfs: *mut sqlite3_vfs, n_byte: c_int, z_err: *mut c_char) {
    unsafe {
        let parent = &*PARENT_VFS;
        if let Some(f) = parent.xDlError {
            f(parent as *const _ as *mut _, n_byte, z_err);
        }
    }
}

unsafe extern "C" fn vfs_x_dl_sym(
    _p_vfs: *mut sqlite3_vfs,
    p_handle: *mut c_void,
    z_sym: *const c_char,
) -> Option<unsafe extern "C" fn(*mut sqlite3_vfs, *mut c_void, *const c_char)> {
    unsafe {
        let parent = &*PARENT_VFS;
        parent
            .xDlSym
            .and_then(|f| f(parent as *const _ as *mut _, p_handle, z_sym))
    }
}

unsafe extern "C" fn vfs_x_dl_close(_p_vfs: *mut sqlite3_vfs, p_handle: *mut c_void) {
    unsafe {
        let parent = &*PARENT_VFS;
        if let Some(f) = parent.xDlClose {
            f(parent as *const _ as *mut _, p_handle);
        }
    }
}

unsafe extern "C" fn vfs_x_randomness(
    _p_vfs: *mut sqlite3_vfs,
    n_byte: c_int,
    z_out: *mut c_char,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xRandomness.unwrap()(parent as *const _ as *mut _, n_byte, z_out)
    }
}

unsafe extern "C" fn vfs_x_sleep(_p_vfs: *mut sqlite3_vfs, us: c_int) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xSleep.unwrap()(parent as *const _ as *mut _, us)
    }
}

unsafe extern "C" fn vfs_x_current_time(_p_vfs: *mut sqlite3_vfs, p_out: *mut f64) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xCurrentTime.unwrap()(parent as *const _ as *mut _, p_out)
    }
}

unsafe extern "C" fn vfs_x_get_last_error(
    _p_vfs: *mut sqlite3_vfs,
    n_byte: c_int,
    z_out: *mut c_char,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xGetLastError.unwrap()(parent as *const _ as *mut _, n_byte, z_out)
    }
}

unsafe extern "C" fn vfs_x_current_time_int64(
    _p_vfs: *mut sqlite3_vfs,
    p_out: *mut sqlite3_int64,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xCurrentTimeInt64.unwrap()(parent as *const _ as *mut _, p_out)
    }
}

unsafe extern "C" fn vfs_x_set_system_call(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
    p_fn: sqlite3_syscall_ptr,
) -> c_int {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xSetSystemCall.unwrap()(parent as *const _ as *mut _, z_name, p_fn)
    }
}

unsafe extern "C" fn vfs_x_get_system_call(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
) -> sqlite3_syscall_ptr {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xGetSystemCall.unwrap()(parent as *const _ as *mut _, z_name)
    }
}

unsafe extern "C" fn vfs_x_next_system_call(
    _p_vfs: *mut sqlite3_vfs,
    z_name: *const c_char,
) -> *const c_char {
    unsafe {
        let parent = &*PARENT_VFS;
        parent.xNextSystemCall.unwrap()(parent as *const _ as *mut _, z_name)
    }
}

fn ensure_fault_vfs_installed() {
    INSTALL_ONCE.call_once(|| unsafe {
        let unix_name = CString::new("unix").unwrap();
        let parent = sqlite3_vfs_find(unix_name.as_ptr());
        assert!(!parent.is_null(), "unix VFS not found");
        PARENT_VFS = parent;
        let name = CString::new("hya-fault").unwrap();
        FAULT_VFS_NAME = name.into_raw();
        let vfs = sqlite3_vfs {
            iVersion: 3, // Hard-coded to the v3 callbacks populated below (never copied from parent).
            szOsFile: std::mem::size_of::<FaultFile>() as c_int,
            mxPathname: (*parent).mxPathname,
            pNext: ptr::null_mut(),
            zName: FAULT_VFS_NAME,
            pAppData: ptr::null_mut(),
            xOpen: Some(vfs_x_open),
            xDelete: Some(vfs_x_delete),
            xAccess: Some(vfs_x_access),
            xFullPathname: Some(vfs_x_full_pathname),
            xDlOpen: Some(vfs_x_dl_open),
            xDlError: Some(vfs_x_dl_error),
            xDlSym: Some(vfs_x_dl_sym),
            xDlClose: Some(vfs_x_dl_close),
            xRandomness: Some(vfs_x_randomness),
            xSleep: Some(vfs_x_sleep),
            xCurrentTime: Some(vfs_x_current_time),
            xGetLastError: Some(vfs_x_get_last_error),
            xCurrentTimeInt64: Some(vfs_x_current_time_int64),
            xSetSystemCall: Some(vfs_x_set_system_call),
            xGetSystemCall: Some(vfs_x_get_system_call),
            xNextSystemCall: Some(vfs_x_next_system_call),
        };
        let vfs_ptr = (&raw mut FAULT_VFS).cast::<sqlite3_vfs>();
        vfs_ptr.write(vfs);
        let rc = sqlite3_vfs_register(vfs_ptr, 1);
        assert_eq!(rc, SQLITE_OK, "sqlite3_vfs_register failed: {rc}");
    });
}

fn create_private_case_dir(dir_name: &str) -> PathBuf {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    #[cfg(target_os = "linux")]
    {
        let shm_candidate = Path::new("/dev/shm").join(dir_name);
        if Path::new("/dev/shm").is_dir() && builder.create(&shm_candidate).is_ok() {
            return shm_candidate;
        }
    }
    let fallback = std::env::temp_dir().join(dir_name);
    builder.create(&fallback).unwrap();
    fallback
}

struct FaultCase {
    dir: PathBuf,
    policy: Arc<FaultPolicy>,
}

impl FaultCase {
    fn new(name: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        ensure_fault_vfs_installed();
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = create_private_case_dir(&format!("hya-fault-{}-{name}-{n}", std::process::id()));
        let policy = FaultPolicy::new();
        REGISTRY
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(dir.clone(), Arc::clone(&policy));
        Self { dir, policy }
    }

    fn db_path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }
}

impl Drop for FaultCase {
    fn drop(&mut self) {
        REGISTRY
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.dir);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn db_error_details(err: &SqlxError) -> Option<(String, String)> {
    if let SqlxError::Database(db) = err {
        Some((
            db.code().map_or_else(|| "none".into(), |c| c.into_owned()),
            db.message().to_string(),
        ))
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn session_store_does_not_retire_connection_after_max_lifetime_or_idle() {
    let _gate = clock_gate().write().await;
    let case = FaultCase::new("no-max-lifetime-retire");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionArchived {
                session,
                archived: 1.into(),
            }],
        )
        .await
        .unwrap();

    // Keeping the read-only pool open holds the WAL alive so SQLite never
    // checkpoints or deletes the WAL unless the writer pool retires its
    // connection and tries to close while the read-only pool is idle.
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let _ = ro.session_exists(session).await.unwrap();
    let reg_db = case.db_path("registry.sqlite3");
    let reg = BundleRegistry::connect(reg_db.to_str().unwrap())
        .await
        .unwrap();
    let _ = reg.generation().await.unwrap();

    // Hold a connection across a single monotonic jump beyond sqlx's 1,800s default max_lifetime.
    let mut tx0 = store.begin_transaction().await.unwrap();
    advance_monotonic_secs(1830);
    case.policy.take_trace();

    hya_store::append_event_in_transaction(
        &mut tx0,
        session,
        Event::SessionTitled {
            session,
            title: "before-drop".into(),
        },
    )
    .await
    .unwrap();
    tx0.commit().await.unwrap();

    // Yield briefly for any background `return_to_pool` close task to start if configured.
    tokio::time::sleep(Duration::from_millis(40)).await;

    let append_res = store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "after-drop".into(),
            }],
        )
        .await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    append_res.expect("append after 1830s jump should succeed");

    let trace = case.policy.take_trace();
    let closed = trace
        .iter()
        .any(|e| e.kind == FileKind::MainDb && e.op == "xClose");
    let locked_excl = trace.iter().any(|e| {
        e.kind == FileKind::MainDb && e.op == "xLock" && e.arg == i64::from(SQLITE_LOCK_EXCLUSIVE)
    });
    let hit_busy = trace
        .iter()
        .any(|e| e.kind == FileKind::MainDb && e.op == "xLock" && e.rc == SQLITE_BUSY);
    assert!(
        !closed && !locked_excl && !hit_busy,
        "unexpected WAL close / retirement activity after 1830s monotonic jump: closed={closed} locked_excl={locked_excl} hit_busy={hit_busy} trace={trace:?}"
    );

    // Also verify read-only store and bundle registry keep connections past max_lifetime.
    case.policy.take_trace();
    let _ = ro.session_exists(session).await.unwrap();
    let _ = reg.generation().await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let trace_ro_reg = case.policy.take_trace();
    assert!(
        !trace_ro_reg
            .iter()
            .any(|e| (e.kind == FileKind::MainDb && e.op == "xClose")
                || (e.kind == FileKind::Wal && e.op == "xDelete")),
        "read-only pool or bundle registry retired a connection: {trace_ro_reg:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundle_registry_set_bundle_enabled_prevents_busy_snapshot() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("bundle-no-busy-snapshot");
    let reg_path = case.db_path("registry.sqlite3");
    let reg = BundleRegistry::connect(reg_path.to_str().unwrap())
        .await
        .unwrap();

    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", reg_path.display()))
        .unwrap()
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(2));
    let helper_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    // Invalidate any warm pcache on `reg`'s pooled connection by committing once
    // from `helper_pool` first, so the next SELECT inside `set_bundle_enabled`
    // must read via VFS `xRead` after locking its read snapshot.
    {
        let mut htx = helper_pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
    }

    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let hp = helper_pool.clone();
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        let mut htx = hp.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
        let _ = done_tx.send(());
    });

    // Skip the single initial page-1 header check inside `lockBtree()` so the
    // hook only fires if the transaction queries table pages *without* holding
    // `WAL_WRITE_LOCK` (deferred `BEGIN`). With `BEGIN IMMEDIATE`, the write
    // lock is acquired immediately after `lockBtree()` and before `SELECT`.
    // Note: this relies on SQLite reading one page before acquiring
    // `WAL_WRITE_LOCK` and routing table reads through `xRead` (non-mmap); a
    // SQLite change fails loudly via `hook_unfired` or the negative control.
    case.policy.arm_shm_read_hook(1, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });

    case.policy.take_trace();
    let outcome = reg.set_bundle_enabled("acme/demo", false).await;
    let hook_unfired = case.policy.clear_shm_read_hook();
    let trace = case.policy.take_trace();
    drop(racer.await);
    outcome.unwrap_or_else(|e| {
        panic!("set_bundle_enabled must use BEGIN IMMEDIATE and avoid SQLITE_BUSY_SNAPSHOT (517): {e}; trace={trace:?}")
    });
    assert!(
        hook_unfired,
        "WAL_WRITE_LOCK was held before querying table data (BEGIN IMMEDIATE)"
    );
    assert!(
        trace.iter().any(|e| {
            e.kind == FileKind::MainDb
                && e.op == "xShmLock"
                && e.offset == 0
                && e.arg == i64::from(SQLITE_SHM_LOCK_EXCL)
                && e.rc == SQLITE_OK
        }),
        "expected BEGIN IMMEDIATE to acquire WAL_WRITE_LOCK: {trace:?}"
    );
    assert!(
        reg.disabled_bundle_ids()
            .await
            .unwrap()
            .contains("acme/demo")
    );

    // Negative control: with the exact same `skip_reads=1` hook, a deferred
    // `.begin()` read-before-write transaction still trips `SQLITE_BUSY_SNAPSHOT` (517).
    let opts_deferred = SqliteConnectOptions::from_str(&format!("sqlite://{}", reg_path.display()))
        .unwrap()
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(2));
    let deferred_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_deferred)
        .await
        .unwrap();
    let _: i64 =
        sqlx::query_scalar("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
            .fetch_one(&deferred_pool)
            .await
            .unwrap();
    {
        let mut htx = helper_pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
    }
    let (ctrl_trig_tx, ctrl_trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (ctrl_done_tx, ctrl_done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let ctrl_hp = helper_pool.clone();
    let ctrl_racer = tokio::spawn(async move {
        if ctrl_trig_rx.await.is_err() {
            return;
        }
        let mut htx = ctrl_hp.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
        let _ = ctrl_done_tx.send(());
    });
    case.policy.arm_shm_read_hook(1, move || {
        let _ = ctrl_trig_tx.send(());
        // Racer writes via a separate connection and cannot block on this one.
        let _ = ctrl_done_rx.recv();
    });
    let mut dtx = deferred_pool.begin().await.unwrap();
    let _: i64 =
        sqlx::query_scalar("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
            .fetch_one(&mut *dtx)
            .await
            .unwrap();
    let hook_fired = !case.policy.clear_shm_read_hook();
    assert!(hook_fired, "deferred SELECT should trigger the xRead hook");
    let ctrl_err = sqlx::query(
        "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
    )
    .execute(&mut *dtx)
    .await
    .expect_err("deferred upgrade after concurrent write must fail with SQLITE_BUSY_SNAPSHOT");
    drop(dtx);
    drop(ctrl_racer.await);
    let (code, _) = db_error_details(&ctrl_err).unwrap();
    assert_eq!(code, "517", "unexpected deferred error: {ctrl_err}");
}
