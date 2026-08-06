//! Validate `crates/hya-e2e/matrix.toml` against the tests that actually exist.
//!
//! The registry used to be inert prose: nothing read it, so it could advertise
//! coverage that was red or absent. Two registered scenarios (`I.nested`,
//! `I.bundle_cli`) were failing for weeks while the registry still listed them
//! as coverage. This check makes that state impossible to reach silently.
//!
//! Deliberately conservative. Bidirectional drift is enforced only for Track P
//! (`crates/hya-e2e/tests`), where the sources are a small, uniformly-styled set
//! of Rust files. Track T is TypeScript and Track I entries are index pointers
//! into other crates that are explicitly *not* meant to map one-to-one onto
//! registry rows — claiming to verify those would produce false failures, and a
//! check that cries wolf gets switched off.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const MATRIX_REL: &str = "crates/hya-e2e/matrix.toml";
const TRACK_P_DIR: &str = "crates/hya-e2e/tests";

#[derive(Debug, Deserialize)]
struct Matrix {
    #[serde(default)]
    scenario: Vec<Scenario>,
    #[serde(default)]
    retired: Vec<Retired>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    id: String,
    title: String,
    track: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct Retired {
    id: String,
    reason: String,
}

/// Entry point for `cargo xtask matrix-check`.
///
/// # Errors
/// Returns an error listing every problem found; exits non-zero via `main`.
pub fn run(_args: Vec<String>) -> Result<()> {
    let root = repo_root()?;
    let matrix_path = root.join(MATRIX_REL);
    let raw = std::fs::read_to_string(&matrix_path)
        .with_context(|| format!("read {}", matrix_path.display()))?;
    let matrix: Matrix =
        toml::from_str(&raw).with_context(|| format!("parse {}", matrix_path.display()))?;

    let mut problems: Vec<String> = Vec::new();

    check_ids(&matrix, &mut problems);
    check_paths_exist(&root, &matrix, &mut problems);
    check_track_p_drift(&root, &matrix, &mut problems);
    check_numbering_gaps(&matrix, &mut problems);

    if problems.is_empty() {
        println!(
            "matrix-check: ok — {} scenario(s), {} retired id(s)",
            matrix.scenario.len(),
            matrix.retired.len()
        );
        return Ok(());
    }

    for problem in &problems {
        eprintln!("matrix-check: {problem}");
    }
    bail!("{} problem(s) in {MATRIX_REL}", problems.len())
}

/// IDs must be unique across scenarios and retirements, and well-formed
/// (`T<major>.<minor>` or `I.<name>`).
fn check_ids(matrix: &Matrix, problems: &mut Vec<String>) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for id in matrix
        .scenario
        .iter()
        .map(|s| s.id.as_str())
        .chain(matrix.retired.iter().map(|r| r.id.as_str()))
    {
        if !seen.insert(id) {
            problems.push(format!("duplicate id `{id}`"));
        }
        if !well_formed_id(id) {
            problems.push(format!(
                "malformed id `{id}` (expected `T<major>.<minor>` or `I.<name>`)"
            ));
        }
    }
    for retired in &matrix.retired {
        if retired.reason.trim().is_empty() {
            problems.push(format!("retired id `{}` has an empty reason", retired.id));
        }
    }
}

fn well_formed_id(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix('I') {
        return rest.strip_prefix('.').is_some_and(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    }
    let Some(rest) = id.strip_prefix('T') else {
        return false;
    };
    let mut parts = rest.split('.');
    let (Some(major), Some(minor), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.chars().all(|c| c.is_ascii_digit())
        && minor.chars().all(|c| c.is_ascii_digit())
}

/// Every registered path must exist. A stale row still reads as coverage.
fn check_paths_exist(root: &Path, matrix: &Matrix, problems: &mut Vec<String>) {
    for scenario in &matrix.scenario {
        if !root.join(&scenario.path).exists() {
            problems.push(format!(
                "`{}` ({}) points at missing path `{}`",
                scenario.id, scenario.title, scenario.path
            ));
        }
    }
}

/// Bidirectional drift for Track P only.
///
/// Correspondence is **file-level, not function-level**: one function can carry
/// several ids (`p01` holds `T0.1` and `T1.2`) and one file can hold several
/// functions (`p03`). A one-to-one rule would fail constantly.
fn check_track_p_drift(root: &Path, matrix: &Matrix, problems: &mut Vec<String>) {
    let mut registered: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for scenario in matrix.scenario.iter().filter(|s| s.track == "P") {
        registered
            .entry(scenario.path.clone())
            .or_default()
            .push(&scenario.id);
    }

    // Forward: each registered Track P file must contain at least one test.
    for (path, ids) in &registered {
        let full = root.join(path);
        if !full.exists() {
            continue; // already reported by check_paths_exist
        }
        match count_tests(&full) {
            Ok(0) => problems.push(format!(
                "`{path}` is registered by {ids:?} but contains no test function"
            )),
            Ok(_) => {}
            Err(error) => problems.push(format!("`{path}`: {error}")),
        }
    }

    // Reverse: every Track P test file must be registered by something.
    let dir = root.join(TRACK_P_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        problems.push(format!("cannot read {}", dir.display()));
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if registered.contains_key(&rel) {
            continue;
        }
        match count_tests(&path) {
            Ok(0) => {}
            Ok(n) => problems.push(format!(
                "`{rel}` holds {n} test function(s) but no matrix entry references it"
            )),
            Err(error) => problems.push(format!("`{rel}`: {error}")),
        }
    }
}

/// Count test functions conservatively: a `fn` / `async fn` preceded within the
/// previous few non-empty lines by an attribute mentioning `test`.
fn count_tests(path: &Path) -> Result<usize> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut count = 0usize;
    let mut saw_test_attr = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("#[") {
            if trimmed.contains("test") {
                saw_test_attr = true;
            }
            continue;
        }
        if saw_test_attr && (trimmed.starts_with("fn ") || trimmed.starts_with("async fn ")) {
            count += 1;
        }
        saw_test_attr = false;
    }
    Ok(count)
}

/// Every id in a `T<major>` series' numeric range must be used or retired.
/// Undeclared holes are how `T1.1` and `T1.6` went untracked for months.
fn check_numbering_gaps(matrix: &Matrix, problems: &mut Vec<String>) {
    let mut used: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for id in matrix
        .scenario
        .iter()
        .map(|s| s.id.as_str())
        .chain(matrix.retired.iter().map(|r| r.id.as_str()))
    {
        if let Some((major, minor)) = parse_t_id(id) {
            used.entry(major).or_default().insert(minor);
        }
    }
    for (major, minors) in &used {
        let Some(&max) = minors.iter().next_back() else {
            continue;
        };
        let gaps: Vec<u32> = (1..=max).filter(|n| !minors.contains(n)).collect();
        if !gaps.is_empty() {
            let list = gaps
                .iter()
                .map(|n| format!("T{major}.{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            problems.push(format!(
                "undeclared numbering gap(s) in the T{major} series: {list} — \
                 define them as scenarios or add a [[retired]] entry with a reason"
            ));
        }
    }
}

fn parse_t_id(id: &str) -> Option<(u32, u32)> {
    let rest = id.strip_prefix('T')?;
    let (major, minor) = rest.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Workspace root, from this crate's manifest dir.
fn repo_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("resolve workspace root from CARGO_MANIFEST_DIR")
}
