//! `cargo xtask target-gc`: delete superseded workspace build artifacts.
//!
//! Cargo never removes an artifact once a newer build replaces it. Every
//! workspace version bump, flag change, or feature-set change gives each
//! workspace crate and test binary a new metadata hash, and the old set stays
//! in `target/<profile>/deps` forever. This task keeps the newest `--keep`
//! units per workspace target and artifact kind, removes every unit of a
//! target the workspace no longer declares, and deletes their `.fingerprint`
//! entries. Third-party artifacts are left alone: they rarely change and
//! several variants (host/target, feature sets) are legitimately live at once.
//!
//! The workspace's own metadata decides what is current, so the task only
//! collects the target directory of the checkout it runs in (`target/`, or
//! `CARGO_TARGET_DIR`). Never point `CARGO_TARGET_DIR` at a directory other
//! checkouts build into: their targets would look undeclared.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};

use crate::metadata::workspace_packages;

const USAGE: &str = "usage: cargo xtask target-gc [--keep N] [--dry-run]";

/// One directory entry of `deps/`.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    modified: SystemTime,
}

#[derive(Debug)]
struct Options {
    keep: usize,
    dry_run: bool,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut options = Self {
            keep: 1,
            dry_run: false,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--dry-run" => options.dry_run = true,
                "--keep" => {
                    let value = args.next().context(USAGE)?;
                    options.keep = value
                        .parse()
                        .with_context(|| format!("--keep {value}: not a number"))?;
                    if options.keep == 0 {
                        bail!("--keep must be at least 1");
                    }
                }
                _ => bail!("unknown argument `{arg}`\n{USAGE}"),
            }
        }
        Ok(options)
    }
}

/// Entry point for `cargo xtask target-gc`.
///
/// # Errors
/// Returns an error when metadata, scanning, or deletion fails.
pub fn run(args: Vec<String>) -> Result<()> {
    let options = Options::parse(args)?;
    let root = crate::gen_api::workspace_root()?;
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));

    let packages: Packages = workspace_packages(&root)?
        .into_iter()
        .map(|package| {
            let names = package.targets.iter().map(|t| t.crate_name()).collect();
            (package.name, names)
        })
        .collect();

    let profiles = profile_dirs(&target_dir)?;
    if profiles.is_empty() {
        println!("target-gc: no build output under {}", target_dir.display());
    }
    for profile in profiles {
        collect_profile(&profile, &packages, &options)?;
    }
    Ok(())
}

/// `<target>/<profile>` and `<target>/<triple>/<profile>` directories that hold
/// a `deps/` directory.
fn profile_dirs(target_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut profiles = Vec::new();
    let Ok(entries) = fs::read_dir(target_dir) else {
        return Ok(profiles);
    };
    for entry in entries {
        let path = entry?.path();
        if path.join("deps").is_dir() {
            profiles.push(path);
        } else if path.is_dir() {
            for nested in fs::read_dir(&path)? {
                let nested = nested?.path();
                if nested.join("deps").is_dir() {
                    profiles.push(nested);
                }
            }
        }
    }
    profiles.sort();
    Ok(profiles)
}

fn collect_profile(profile: &Path, packages: &Packages, options: &Options) -> Result<()> {
    // Cargo holds this lock while it builds into the profile directory; taking
    // it keeps the collector from deleting outputs of a build in progress.
    let lock_path = profile.join(".cargo-lock");
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            println!(
                "target-gc: waiting for cargo to release {}",
                lock_path.display()
            );
            lock.lock()
                .with_context(|| format!("lock {}", lock_path.display()))?;
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("lock {}", lock_path.display()));
        }
    }

    let deps = read_entries(&profile.join("deps"))?;
    let fingerprints = read_entries(&profile.join(".fingerprint"))?
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    let plan = plan_removals(&deps, &fingerprints, packages, options.keep);

    let mut bytes = 0u64;
    for relative in &plan {
        let path = profile.join(relative);
        bytes += disk_size(&path);
        if options.dry_run {
            println!("would remove {}", path.display());
        } else if path.is_dir() {
            fs::remove_dir_all(&path).with_context(|| format!("remove {}", path.display()))?;
        } else {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    let verb = if options.dry_run {
        "would free"
    } else {
        "freed"
    };
    println!(
        "target-gc: {}: {verb} {:.2} GiB in {} entries",
        profile.display(),
        bytes as f64 / f64::from(1u32 << 30),
        plan.len()
    );
    Ok(())
}

fn read_entries(dir: &Path) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let Ok(listing) = fs::read_dir(dir) else {
        return Ok(entries);
    };
    for entry in listing {
        let entry = entry.with_context(|| format!("read {}", dir.display()))?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("stat {}", entry.path().display()))?;
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    Ok(entries)
}

/// Bytes a file or directory tree occupies; unreadable parts count as zero.
fn disk_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| disk_size(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// Split `[lib]<stem>-<16 hex>[.<suffix>]` into `(stem, hash, suffix)`.
/// The `lib` prefix, if any, stays on the stem.
fn split_unit(name: &str) -> Option<(&str, &str, &str)> {
    for (dash, _) in name.match_indices('-') {
        let start = dash + 1;
        let Some(hash) = name.get(start..start + 16) else {
            break;
        };
        let rest = &name[start + 16..];
        if hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && (rest.is_empty() || rest.starts_with('.'))
        {
            return Some((&name[..dash], hash, rest));
        }
    }
    None
}

/// What a unit (one stem + hash) produced; units of different kinds for the
/// same target are independent builds (`cargo check` vs `cargo test`, the
/// library vs its unit-test harness) and never evict each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Rlib,
    Dylib,
    Executable,
    Metadata,
    Partial,
}

fn unit_kind(suffixes: &BTreeSet<&str>) -> Kind {
    if suffixes.contains(".rlib") {
        Kind::Rlib
    } else if [".dylib", ".so", ".dll"]
        .iter()
        .any(|s| suffixes.contains(s))
    {
        Kind::Dylib
    } else if suffixes.contains("") || suffixes.contains(".exe") {
        Kind::Executable
    } else if suffixes.contains(".rmeta") {
        Kind::Metadata
    } else {
        Kind::Partial
    }
}

/// A unit's newest modification time and its hash.
type Member<'a> = (Option<SystemTime>, &'a str);

#[derive(Default)]
struct Unit<'a> {
    names: Vec<&'a str>,
    suffixes: BTreeSet<&'a str>,
    newest: Option<SystemTime>,
}

/// Workspace package name -> crate names of the targets it declares today.
type Packages = BTreeMap<String, BTreeSet<String>>;

/// Paths, relative to the profile directory, of superseded workspace units.
///
/// A unit (one metadata hash) belongs to the package named by its
/// `.fingerprint/<package>-<hash>` directory; units of other packages are
/// third-party and never touched, as are units without a fingerprint. Per
/// package, target, and [`Kind`] the `keep` most recently built units stay;
/// units of a target the package no longer declares (a renamed or merged test
/// file) are all removed. Removed units take their fingerprints with them.
fn plan_removals(
    deps: &[Entry],
    fingerprints: &[String],
    packages: &Packages,
    keep: usize,
) -> Vec<PathBuf> {
    let owners: BTreeMap<&str, &str> = fingerprints
        .iter()
        .filter_map(|name| split_unit(name))
        .filter(|(package, _, rest)| rest.is_empty() && packages.contains_key(*package))
        .map(|(package, hash, _)| (hash, package))
        .collect();

    let mut units: BTreeMap<(&str, &str, &str), Unit<'_>> = BTreeMap::new();
    for entry in deps {
        let Some((stem, hash, suffix)) = split_unit(&entry.name) else {
            continue;
        };
        let Some(package) = owners.get(hash).copied() else {
            continue;
        };
        let names = packages.get(package);
        let stem = match stem.strip_prefix("lib") {
            Some(bare) if names.is_some_and(|n| n.contains(bare) && !n.contains(stem)) => bare,
            _ => stem,
        };
        let unit = units.entry((package, stem, hash)).or_default();
        unit.names.push(&entry.name);
        unit.suffixes.insert(suffix);
        unit.newest = unit.newest.max(Some(entry.modified));
    }

    // (package, target, kind) -> [(newest mtime, hash)] of its units.
    let mut groups: BTreeMap<(&str, &str, Kind), Vec<Member<'_>>> = BTreeMap::new();
    for ((package, stem, hash), unit) in &units {
        groups
            .entry((package, stem, unit_kind(&unit.suffixes)))
            .or_default()
            .push((unit.newest, hash));
    }

    let mut removed_hashes: BTreeSet<&str> = BTreeSet::new();
    let mut plan: Vec<PathBuf> = Vec::new();
    for ((package, stem, _), mut members) in groups {
        let declared = packages
            .get(package)
            .is_some_and(|names| names.contains(stem));
        let allowed = if declared { keep } else { 0 };
        members.sort_by(|a, b| b.cmp(a));
        for (_, hash) in members.into_iter().skip(allowed) {
            removed_hashes.insert(hash);
            if let Some(unit) = units.get(&(package, stem, hash)) {
                plan.extend(unit.names.iter().map(|name| Path::new("deps").join(name)));
            }
        }
    }
    plan.extend(
        fingerprints
            .iter()
            .filter(|name| {
                split_unit(name)
                    .is_some_and(|(_, hash, rest)| rest.is_empty() && removed_hashes.contains(hash))
            })
            .map(|name| Path::new(".fingerprint").join(name)),
    );
    plan.sort();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const LIB_OLD: &str = "000000000000a001";
    const LIB_NEW: &str = "000000000000a002";
    const IT_OLD: &str = "000000000000b001";
    const IT_NEW: &str = "000000000000b002";

    fn entry(name: &str, secs: u64) -> Entry {
        Entry {
            name: name.to_string(),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    fn hya_core() -> Packages {
        BTreeMap::from([(
            "hya-core".to_string(),
            BTreeSet::from(["hya_core".to_string(), "hya_core_it".to_string()]),
        )])
    }

    fn fingerprints(package: &str, hashes: &[&str]) -> Vec<String> {
        hashes.iter().map(|h| format!("{package}-{h}")).collect()
    }

    fn removed(plan: &[PathBuf]) -> BTreeSet<String> {
        plan.iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn superseded_units_are_removed_with_their_fingerprints() {
        let deps = [
            entry(&format!("libhya_core-{LIB_OLD}.rlib"), 10),
            entry(&format!("libhya_core-{LIB_OLD}.rmeta"), 10),
            entry(&format!("hya_core-{LIB_OLD}.d"), 10),
            entry(&format!("libhya_core-{LIB_NEW}.rlib"), 20),
            entry(&format!("libhya_core-{LIB_NEW}.rmeta"), 20),
            entry(&format!("hya_core_it-{IT_OLD}"), 11),
            entry(
                &format!("hya_core_it-{IT_OLD}.hya_core_it.a1-cgu.0.rcgu.o"),
                11,
            ),
            entry(&format!("hya_core_it-{IT_NEW}"), 21),
        ];
        let fingerprints = fingerprints("hya-core", &[LIB_OLD, LIB_NEW, IT_OLD, IT_NEW]);
        let plan = plan_removals(&deps, &fingerprints, &hya_core(), 1);
        let expected: BTreeSet<String> = [
            format!("deps/libhya_core-{LIB_OLD}.rlib"),
            format!("deps/libhya_core-{LIB_OLD}.rmeta"),
            format!("deps/hya_core-{LIB_OLD}.d"),
            format!("deps/hya_core_it-{IT_OLD}"),
            format!("deps/hya_core_it-{IT_OLD}.hya_core_it.a1-cgu.0.rcgu.o"),
            format!(".fingerprint/hya-core-{LIB_OLD}"),
            format!(".fingerprint/hya-core-{IT_OLD}"),
        ]
        .into_iter()
        .collect();
        assert_eq!(removed(&plan), expected);
    }

    #[test]
    fn third_party_and_unowned_artifacts_are_never_removed() {
        let deps = [
            entry(&format!("libserde-{LIB_OLD}.rlib"), 10),
            entry(&format!("libserde-{LIB_NEW}.rlib"), 20),
            // A workspace-looking name without a fingerprint: ownership unknown.
            entry(&format!("hya_core_it-{IT_OLD}"), 10),
            entry(&format!("hya_core_it-{IT_NEW}"), 20),
        ];
        let fingerprints = fingerprints("serde", &[LIB_OLD, LIB_NEW]);
        let plan = plan_removals(&deps, &fingerprints, &hya_core(), 1);
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn check_only_and_full_builds_are_kept_independently() {
        // `cargo clippy` leaves an rmeta-only unit; `cargo test` an rlib. Both
        // are live, so neither may evict the other.
        let deps = [
            entry(&format!("libhya_core-{LIB_OLD}.rlib"), 10),
            entry(&format!("libhya_core-{LIB_OLD}.rmeta"), 10),
            entry(&format!("libhya_core-{LIB_NEW}.rmeta"), 20),
        ];
        let fingerprints = fingerprints("hya-core", &[LIB_OLD, LIB_NEW]);
        let plan = plan_removals(&deps, &fingerprints, &hya_core(), 1);
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn targets_the_package_no_longer_declares_are_removed_entirely() {
        // `subagent` was a per-file test binary before the tests were merged.
        let deps = [
            entry(&format!("subagent-{IT_OLD}"), 10),
            entry(&format!("subagent-{IT_OLD}.d"), 10),
            entry(&format!("hya_core_it-{IT_NEW}"), 20),
        ];
        let fingerprints = fingerprints("hya-core", &[IT_OLD, IT_NEW]);
        let plan = plan_removals(&deps, &fingerprints, &hya_core(), 1);
        assert_eq!(
            removed(&plan),
            BTreeSet::from([
                format!("deps/subagent-{IT_OLD}"),
                format!("deps/subagent-{IT_OLD}.d"),
                format!(".fingerprint/hya-core-{IT_OLD}"),
            ])
        );
    }
}
