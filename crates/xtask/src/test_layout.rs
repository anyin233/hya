//! `cargo xtask test-layout`: every `tests/*.rs` file must be compiled.
//!
//! Crates with several integration-test files set `autotests = false` and
//! compile them as modules of one `tests/main.rs` binary, so the crate links
//! once instead of once per file. The cost is that a new `tests/foo.rs` that
//! nobody adds to `tests/main.rs` (or to a `[[test]]` entry) is silently never
//! built or run. This check turns that into a CI failure.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::metadata::workspace_packages;

/// Entry point for `cargo xtask test-layout`.
///
/// # Errors
/// Returns an error naming every `tests/*.rs` file no test target compiles.
pub fn run(_args: Vec<String>) -> Result<()> {
    let root = crate::gen_api::workspace_root()?;
    let mut problems = Vec::new();
    let mut checked = 0usize;
    for package in workspace_packages(&root)? {
        let Some(dir) = package.manifest_path.parent() else {
            continue;
        };
        let tests_dir = dir.join("tests");
        let Ok(listing) = std::fs::read_dir(&tests_dir) else {
            continue;
        };
        let mut files = Vec::new();
        for entry in listing {
            let path = entry
                .with_context(|| format!("read {}", tests_dir.display()))?
                .path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        files.sort();
        let mut roots = Vec::new();
        for target in package
            .targets
            .iter()
            .filter(|t| t.kind.iter().any(|k| k == "test"))
        {
            let source = std::fs::read_to_string(&target.src_path)
                .with_context(|| format!("read {}", target.src_path.display()))?;
            roots.push((target.src_path.clone(), source));
        }
        checked += files.len();
        problems.extend(unregistered(&files, &roots));
    }

    if problems.is_empty() {
        println!("test-layout: ok — {checked} integration-test file(s) compiled");
        return Ok(());
    }
    for path in &problems {
        let shown = path.strip_prefix(&root).unwrap_or(path);
        eprintln!(
            "test-layout: `{}` is not compiled: add `mod <name>;` to the crate's \
             tests/main.rs or a `[[test]]` entry to its Cargo.toml",
            shown.display()
        );
    }
    bail!(
        "{} integration-test file(s) are never compiled",
        problems.len()
    )
}

/// Files in `files` that are neither a test-target root in `roots` nor a
/// module (`mod name;` or `#[path = "…"] mod name;`) declared by one.
fn unregistered(files: &[PathBuf], roots: &[(PathBuf, String)]) -> Vec<PathBuf> {
    let mut compiled: BTreeSet<PathBuf> = BTreeSet::new();
    for (root, source) in roots {
        compiled.insert(root.clone());
        let dir = root.parent().unwrap_or(Path::new(""));
        let mut path_attr: Option<&str> = None;
        for line in source.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("#[path = \"") {
                path_attr = rest.strip_suffix("\"]");
                continue;
            }
            let declaration = line.strip_prefix("pub ").unwrap_or(line);
            if let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
            {
                let file = path_attr.map_or_else(|| format!("{name}.rs"), str::to_string);
                compiled.insert(dir.join(file));
            }
            path_attr = None;
        }
    }
    files
        .iter()
        .filter(|file| !compiled.contains(*file))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|n| Path::new("/w/tests").join(n))
            .collect()
    }

    #[test]
    fn files_outside_every_test_target_are_reported() {
        let files = paths(&["main.rs", "alpha.rs", "beta.rs", "common.rs", "env.rs"]);
        let roots = [
            (
                PathBuf::from("/w/tests/main.rs"),
                "//! docs\n\nmod common;\npub mod alpha;\n".to_string(),
            ),
            (
                PathBuf::from("/w/tests/env.rs"),
                "mod support;\n".to_string(),
            ),
        ];
        assert_eq!(unregistered(&files, &roots), paths(&["beta.rs"]));
    }

    #[test]
    fn a_path_attribute_module_registers_its_file_not_its_name() {
        let files = paths(&["main.rs", "script.rs", "event_script.rs"]);
        let roots = [(
            PathBuf::from("/w/tests/main.rs"),
            "#[path = \"script.rs\"]\nmod event_script;\n".to_string(),
        )];
        assert_eq!(unregistered(&files, &roots), paths(&["event_script.rs"]));
    }
}
