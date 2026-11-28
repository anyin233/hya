//! Core rings (ADR-0040): R0 context kernel ← R1 injection host ← R2 serving
//! surface. One crate-to-ring map in `crates/xtask/boundary.toml` drives both
//! the dependency-direction rule (part of the boundary guard) and the
//! per-ring size report (`cargo run -p xtask -- ring-report`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, bail};
use serde::Deserialize;

/// Where each core directory sits. Entries are directories relative to the
/// workspace root (`crates/<name>`, `packages/<name>`, `proto`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rings {
    r0: Vec<String>,
    r1: Vec<String>,
    r2: Vec<String>,
    /// The `hya` executable: composes R2 with bundle tooling and the client
    /// library; counted in R2, and nothing may depend on it.
    entry: Vec<String>,
    clients: Vec<String>,
    /// Bundle authoring/installation tooling (not core).
    bundle_tooling: Vec<String>,
    /// Repository tooling and test harnesses (not core).
    repo_tooling: Vec<String>,
    /// Files counted on their own size row instead of their ring's.
    #[serde(default)]
    rows: BTreeMap<String, Vec<String>>,
    /// Paths left out of the size report (duplicated sources).
    #[serde(default)]
    exclude: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Ring(u8),
    Entry,
    Client,
    BundleTooling,
    RepoTooling,
}

impl Place {
    /// Whether code in `self` may depend on code in `dep`.
    fn may_depend_on(self, dep: Place) -> bool {
        match (self, dep) {
            (Place::Ring(own), Place::Ring(other)) => other <= own,
            (Place::Ring(_), _) => false,
            (Place::Entry, _) => dep != Place::Entry,
            (Place::Client, Place::Ring(_) | Place::Client) => true,
            (Place::Client, _) => false,
            (Place::BundleTooling, Place::Ring(_) | Place::BundleTooling) => true,
            (Place::BundleTooling, _) => false,
            (Place::RepoTooling, _) => true,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Place::Ring(0) => "R0",
            Place::Ring(1) => "R1",
            Place::Ring(_) | Place::Entry => "R2",
            Place::Client => "clients",
            Place::BundleTooling => "bundle tooling",
            Place::RepoTooling => "repo tooling",
        }
    }
}

impl Rings {
    fn places(&self) -> BTreeMap<&str, Place> {
        let groups = [
            (&self.r0, Place::Ring(0)),
            (&self.r1, Place::Ring(1)),
            (&self.r2, Place::Ring(2)),
            (&self.entry, Place::Entry),
            (&self.clients, Place::Client),
            (&self.bundle_tooling, Place::BundleTooling),
            (&self.repo_tooling, Place::RepoTooling),
        ];
        groups
            .into_iter()
            .flat_map(|(dirs, place)| dirs.iter().map(move |dir| (dir.as_str(), place)))
            .collect()
    }
}

/// Ring-direction violations: crate directory → workspace crates it may not
/// depend on (normal and build dependencies). Every directory under `crates/`
/// and `packages/` must be mapped, once.
pub fn violations(
    root: &Path,
    rings: &Rings,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let places = rings.places();
    let mapped: usize = [
        &rings.r0,
        &rings.r1,
        &rings.r2,
        &rings.entry,
        &rings.clients,
        &rings.bundle_tooling,
        &rings.repo_tooling,
    ]
    .iter()
    .map(|dirs| dirs.len())
    .sum();
    if mapped != places.len() {
        bail!("a directory is mapped to more than one ring in [rings]");
    }
    let mut dirs = Vec::new();
    for parent in ["crates", "packages"] {
        for entry in
            std::fs::read_dir(root.join(parent)).with_context(|| format!("read {parent}/"))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(format!("{parent}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    let unmapped: Vec<_> = dirs
        .iter()
        .filter(|d| !places.contains_key(d.as_str()))
        .collect();
    if !unmapped.is_empty() {
        bail!("not in any [rings] group: {unmapped:?}");
    }
    // Package name → its directory's place.
    let mut by_name = BTreeMap::new();
    let mut manifests = Vec::new();
    for dir in dirs.iter().filter(|d| d.starts_with("crates/")) {
        let manifest = root.join(dir).join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let value: toml::Value = toml::from_str(&std::fs::read_to_string(&manifest)?)
            .with_context(|| format!("parse {}", manifest.display()))?;
        if let Some(name) = value
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
        {
            by_name.insert(name.to_owned(), places[dir.as_str()]);
        }
        manifests.push((dir.clone(), value));
    }
    let mut found = BTreeMap::new();
    for (dir, value) in manifests {
        let own = places[dir.as_str()];
        let mut bad = BTreeSet::new();
        for table in ["dependencies", "build-dependencies"]
            .iter()
            .filter_map(|key| value.get(key).and_then(toml::Value::as_table))
        {
            for (key, spec) in table {
                let name = spec
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                if let Some(&dep) = by_name.get(name)
                    && !own.may_depend_on(dep)
                {
                    bad.insert(name.to_owned());
                }
            }
        }
        if !bad.is_empty() {
            found.insert(dir, bad);
        }
    }
    Ok(found)
}

/// `cargo run -p xtask -- ring-report`: production lines per ring.
pub fn report(root: &Path, rings: &Rings) -> anyhow::Result<String> {
    let places = rings.places();
    let files = Command::new("git")
        .args(["ls-files", "crates", "packages", "proto"])
        .current_dir(root)
        .output()
        .context("git ls-files")?;
    let mut rows: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for path in String::from_utf8(files.stdout)?.lines() {
        let source = ["rs", "ts", "tsx", "proto"]
            .iter()
            .any(|ext| path.ends_with(&format!(".{ext}")));
        if !source
            || rings.exclude.iter().any(|e| path.starts_with(e.as_str()))
            || crate::boundary::is_test_or_generated(Path::new(path))
        {
            continue;
        }
        let dir = places
            .keys()
            .filter(|dir| path.starts_with(&format!("{dir}/")))
            .max_by_key(|dir| dir.len());
        let Some(dir) = dir else { continue };
        let row = rings
            .rows
            .iter()
            .find(|(_, files)| files.iter().any(|f| f == path))
            .map_or_else(|| places[dir].label().to_owned(), |(row, _)| row.clone());
        let text =
            std::fs::read_to_string(root.join(path)).with_context(|| format!("read {path}"))?;
        let entry = rows.entry(row).or_default();
        entry.0 += 1;
        entry.1 += production_lines(&text, path.ends_with(".rs"));
    }
    let order = ["R0", "R1", "R2"];
    let mut out = String::from("| Row | Files | Production lines |\n| --- | ---: | ---: |\n");
    let mut core = 0;
    for name in order {
        let (files, lines) = rows.remove(name).unwrap_or_default();
        core += lines;
        out.push_str(&format!("| {name} | {files} | {lines} |\n"));
    }
    out.push_str(&format!("| R0 + R1 + R2 | | {core} |\n"));
    for (name, (files, lines)) in rows {
        out.push_str(&format!("| {name} | {files} | {lines} |\n"));
    }
    Ok(out)
}

/// Non-blank, non-comment lines outside `#[cfg(test)]` modules.
fn production_lines(text: &str, rust: bool) -> usize {
    let mut count = 0;
    let mut in_block_comment = false;
    let mut cfg_test = false;
    let mut test_depth: Option<i64> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if in_block_comment {
            in_block_comment = !line.contains("*/");
            continue;
        }
        if line.starts_with("/*") {
            in_block_comment = !line.contains("*/");
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        if let Some(depth) = test_depth.as_mut() {
            *depth += braces(line);
            if *depth <= 0 {
                test_depth = None;
            }
            continue;
        }
        if rust && line.starts_with("#[cfg(test)]") {
            cfg_test = true;
            continue;
        }
        if cfg_test {
            if line.starts_with("#[") {
                continue;
            }
            cfg_test = false;
            let module = line
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            if module.starts_with("mod ") {
                let depth = braces(line);
                if depth > 0 {
                    test_depth = Some(depth);
                }
                continue;
            }
        }
        count += 1;
    }
    count
}

fn braces(line: &str) -> i64 {
    let opens = line.matches('{').count();
    let closes = line.matches('}').count();
    i64::try_from(opens).unwrap_or(0) - i64::try_from(closes).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_code_but_not_comments_blanks_or_test_modules() {
        let src = "// c\n/* a\n b */\nfn a() {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn b() {}\n";
        assert_eq!(production_lines(src, true), 4);
    }

    #[test]
    fn inner_ring_must_not_depend_outward() {
        assert!(Place::Ring(1).may_depend_on(Place::Ring(0)));
        assert!(!Place::Ring(0).may_depend_on(Place::Ring(1)));
        assert!(!Place::Ring(2).may_depend_on(Place::Client));
        assert!(!Place::Ring(1).may_depend_on(Place::BundleTooling));
        assert!(Place::Entry.may_depend_on(Place::BundleTooling));
        assert!(!Place::Ring(2).may_depend_on(Place::Entry));
    }
}
