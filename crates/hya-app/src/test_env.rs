//! One gate for unit tests that read or change process environment and cwd.

#![allow(clippy::unwrap_used)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

static ENV_LOCK: RwLock<()> = RwLock::new(());

pub(crate) struct StableEnvGuard {
    _lock: RwLockReadGuard<'static, ()>,
}

impl StableEnvGuard {
    pub(crate) fn new() -> Self {
        Self {
            _lock: ENV_LOCK
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
}

pub(crate) struct EnvGuard {
    _lock: RwLockWriteGuard<'static, ()>,
    saved: Vec<(String, Option<OsString>)>,
    current_dir: Option<PathBuf>,
}

impl EnvGuard {
    pub(crate) fn new() -> Self {
        Self {
            _lock: ENV_LOCK
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            saved: Vec::new(),
            current_dir: None,
        }
    }

    pub(crate) fn with_home(home: &Path, cwd: &Path) -> Self {
        let mut guard = Self::new();
        guard.current_dir = Some(std::env::current_dir().unwrap());
        std::fs::create_dir_all(home).unwrap();
        std::fs::create_dir_all(cwd).unwrap();
        guard.set("HOME", home);
        guard.set("XDG_CONFIG_HOME", home);
        guard.set("XDG_DATA_HOME", home);
        std::env::set_current_dir(cwd).unwrap();
        guard
    }

    fn save(&mut self, name: &str) {
        if !self.saved.iter().any(|(key, _)| key == name) {
            self.saved.push((name.to_string(), std::env::var_os(name)));
        }
    }

    pub(crate) fn set(&mut self, name: &str, value: impl AsRef<OsStr>) {
        self.save(name);
        unsafe { std::env::set_var(name, value) };
    }

    pub(crate) fn remove(&mut self, name: &str) {
        self.save(name);
        unsafe { std::env::remove_var(name) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(cwd) = &self.current_dir {
            std::env::set_current_dir(cwd).unwrap();
        }
        for (name, previous) in self.saved.iter().rev() {
            match previous {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}
