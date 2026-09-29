//! Generation pinning for restart rollback (ADR-0028 amendment "Rollback to
//! the pinned generation").
//!
//! A running daemon copies the executable it runs and the native tool
//! libraries/packages it loaded into `<db>.server.gen/<pid>/`, in the same
//! relative layout, so the copied executable resolves the same libraries. A
//! Cargo build also loads the in-tree first-party bundle sources; those are
//! copied to `<pin>/first-party/` and a rollback points the pinned build at
//! them (`HYA_FIRST_PARTY_SOURCE_ROOT`).
//! Why: a rebuild (`cargo build`) or an update replaces those files in place,
//! so after a failed restart the old generation's own `current_exe()` path
//! may already be the failed build. The pin is what a failed handoff rolls
//! back to. The copy uses `std::fs::copy` (a copy-on-write clone on APFS and
//! reflink-capable filesystems). The pin is removed when the daemon exits;
//! pins of dead pids are swept when the next one is made.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// One pinned generation; the directory is removed on drop.
#[derive(Debug)]
pub(crate) struct GenerationPin {
    dir: PathBuf,
    exe: PathBuf,
    first_party_root: Option<PathBuf>,
}

impl GenerationPin {
    /// The pinned copy of the running executable.
    pub(crate) fn exe(&self) -> &Path {
        &self.exe
    }

    /// The pinned first-party source root (same `presets/`/`first-party/`
    /// layout as the tree), when this build loaded in-tree sources.
    pub(crate) fn first_party_root(&self) -> Option<&Path> {
        self.first_party_root.as_deref()
    }
}

/// One in-tree first-party bundle directory this build loaded, and its path
/// relative to the first-party source root.
#[derive(Debug)]
pub(crate) struct FirstPartyDir {
    pub(crate) source: PathBuf,
    pub(crate) relative: PathBuf,
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `<db>.server.gen`: the pin root of a database.
pub(crate) fn pin_root(db: &str) -> PathBuf {
    let mut root = std::ffi::OsString::from(db);
    root.push(".server.gen");
    PathBuf::from(root)
}

/// Pin the running generation of the daemon of `db`.
pub(crate) fn pin(db: &str) -> anyhow::Result<GenerationPin> {
    let exe = std::env::current_exe().context("find the running executable")?;
    let libraries = hya_tool::native_bundle::loaded_library_sources();
    let exe_dir = exe.parent().context("the executable has no directory")?;
    let root = hya_bundle::first_party_source_root();
    let first_party: Vec<FirstPartyDir> = hya_bundle::FIRST_PARTY_BUNDLES
        .iter()
        .filter_map(
            |identity| match hya_bundle::first_party_source(exe_dir, identity)? {
                hya_bundle::FirstPartySource::Directory(source) => Some(source),
                hya_bundle::FirstPartySource::Package(_) => None,
            },
        )
        .filter_map(|source| {
            let relative = source.strip_prefix(&root).ok()?.to_path_buf();
            Some(FirstPartyDir { source, relative })
        })
        .collect();
    pin_files(
        &pin_root(db),
        std::process::id(),
        &exe,
        &libraries,
        &first_party,
    )
}

/// Copy `exe` and the loaded `libraries` into `root/<pid>/`, keeping the
/// layout the native and first-party loaders resolve from: an installed
/// `bin/hya` keeps `bin/` and the sibling `bundles/` packages; a Cargo
/// layout keeps the executable and its libraries side by side.
pub(crate) fn pin_files(
    root: &Path,
    pid: u32,
    exe: &Path,
    libraries: &[PathBuf],
    first_party: &[FirstPartyDir],
) -> anyhow::Result<GenerationPin> {
    sweep(root);
    let dir = root.join(pid.to_string());
    let _ = std::fs::remove_dir_all(&dir);
    let exe_dir = exe.parent().context("the executable has no directory")?;
    let file_name = exe.file_name().context("the executable has no file name")?;
    let installed = exe_dir.file_name().and_then(std::ffi::OsStr::to_str) == Some("bin");
    let pinned_dir = if installed {
        dir.join("bin")
    } else {
        dir.clone()
    };
    std::fs::create_dir_all(&pinned_dir)
        .with_context(|| format!("create {}", pinned_dir.display()))?;
    let pinned_exe = pinned_dir.join(file_name);
    let first_party_root = (!first_party.is_empty()).then(|| dir.join("first-party"));
    let pin = GenerationPin {
        dir: dir.clone(),
        exe: pinned_exe.clone(),
        first_party_root: first_party_root.clone(),
    };
    if let Some(root) = &first_party_root {
        for bundle in first_party {
            copy_tree(&bundle.source, &root.join(&bundle.relative))?;
        }
    }
    copy(exe, &pinned_exe)?;
    for library in libraries {
        let Some(name) = library.file_name() else {
            continue;
        };
        let is_package = library
            .extension()
            .is_some_and(|extension| extension == "hyabundle");
        let target = if is_package {
            dir.join("bundles").join(name)
        } else {
            pinned_dir.join(name)
        };
        copy(library, &target)?;
    }
    if installed && let Some(prefix) = exe_dir.parent() {
        // The installed layout loads first-party bundles from packages
        // beside `bin/`; pin them all (they are small).
        if let Ok(entries) = std::fs::read_dir(prefix.join("bundles")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "hyabundle")
                {
                    copy(&path, &dir.join("bundles").join(entry.file_name()))?;
                }
            }
        }
    }
    Ok(pin)
}

fn copy(from: &Path, to: &Path) -> anyhow::Result<()> {
    if to.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .with_context(|| format!("pin {} as {}", from.display(), to.display()))?;
    Ok(())
}

/// Copy a bundle source directory, skipping build output and dependencies.
fn copy_tree(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some("target" | "node_modules" | ".git")) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() {
            copy(&entry.path(), &to.join(&name))?;
        }
    }
    Ok(())
}

/// Remove the pins of processes that no longer run.
fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none_or(|pid| !crate::daemon::process_alive(pid));
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hya-pin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_cargo_layout_pins_the_exe_beside_its_libraries() {
        let root = scratch("cargo");
        let build = root.join("target/debug");
        std::fs::create_dir_all(build.join("deps")).unwrap();
        std::fs::write(build.join("hya"), b"old exe").unwrap();
        std::fs::write(build.join("deps/libhya_base_tools.dylib"), b"old lib").unwrap();
        let tree = root.join("bundles");
        std::fs::create_dir_all(tree.join("presets/core-agents/prompts")).unwrap();
        std::fs::write(
            tree.join("presets/core-agents/bundle.yaml"),
            b"old manifest",
        )
        .unwrap();
        std::fs::write(tree.join("presets/core-agents/prompts/a.md"), b"old prompt").unwrap();
        std::fs::create_dir_all(tree.join("presets/core-agents/target/debug")).unwrap();
        std::fs::write(tree.join("presets/core-agents/target/debug/junk"), b"x").unwrap();
        let pins = root.join("s.db.server.gen");
        let pin = pin_files(
            &pins,
            4242,
            &build.join("hya"),
            &[build.join("deps/libhya_base_tools.dylib")],
            &[FirstPartyDir {
                source: tree.join("presets/core-agents"),
                relative: PathBuf::from("presets/core-agents"),
            }],
        )
        .unwrap();
        std::fs::write(
            tree.join("presets/core-agents/prompts/a.md"),
            b"edited prompt",
        )
        .unwrap();
        // The in-tree first-party sources this build loaded are pinned too;
        // a rollback points the pinned build at them.
        let first_party = pin.first_party_root().unwrap();
        assert_eq!(first_party, pins.join("4242/first-party"));
        assert_eq!(
            std::fs::read(first_party.join("presets/core-agents/prompts/a.md")).unwrap(),
            b"old prompt"
        );
        assert!(!first_party.join("presets/core-agents/target").exists());
        // A rebuild replaces the originals; the pin keeps the old ones.
        std::fs::write(build.join("hya"), b"new exe").unwrap();
        std::fs::write(build.join("deps/libhya_base_tools.dylib"), b"new lib").unwrap();
        assert_eq!(pin.exe(), pins.join("4242/hya"));
        assert_eq!(std::fs::read(pin.exe()).unwrap(), b"old exe");
        // `library_source` looks beside the executable.
        assert_eq!(
            std::fs::read(pins.join("4242/libhya_base_tools.dylib")).unwrap(),
            b"old lib"
        );
        drop(pin);
        assert!(!pins.join("4242").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_installed_layout_pins_bin_and_the_bundle_packages() {
        let root = scratch("installed");
        let prefix = root.join("lib/hya");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::create_dir_all(prefix.join("bundles")).unwrap();
        std::fs::write(prefix.join("bin/hya"), b"exe").unwrap();
        std::fs::write(prefix.join("bundles/hya-core-agents.hyabundle"), b"agents").unwrap();
        std::fs::write(prefix.join("bundles/hya-base-tools.hyabundle"), b"tools").unwrap();
        let pins = root.join("s.db.server.gen");
        let pin = pin_files(
            &pins,
            7,
            &prefix.join("bin/hya"),
            &[prefix.join("bundles/hya-base-tools.hyabundle")],
            &[],
        )
        .unwrap();
        assert_eq!(pin.first_party_root(), None, "packages are pinned instead");
        assert_eq!(pin.exe(), pins.join("7/bin/hya"));
        assert_eq!(
            std::fs::read(pins.join("7/bundles/hya-core-agents.hyabundle")).unwrap(),
            b"agents"
        );
        assert_eq!(
            std::fs::read(pins.join("7/bundles/hya-base-tools.hyabundle")).unwrap(),
            b"tools"
        );
        drop(pin);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pins_of_dead_processes_are_swept() {
        let root = scratch("sweep");
        let pins = root.join("s.db.server.gen");
        // pid 0x7fff_fff0 does not run; a non-numeric entry is stale too.
        std::fs::create_dir_all(pins.join("2147483632")).unwrap();
        std::fs::create_dir_all(pins.join("junk")).unwrap();
        std::fs::write(root.join("hya"), b"exe").unwrap();
        let pin = pin_files(&pins, std::process::id(), &root.join("hya"), &[], &[]).unwrap();
        assert!(!pins.join("2147483632").exists());
        assert!(!pins.join("junk").exists());
        assert!(pin.exe().is_file());
        drop(pin);
        let _ = std::fs::remove_dir_all(root);
    }
}
