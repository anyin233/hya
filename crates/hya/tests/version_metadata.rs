use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn release_metadata_matches_hya_package_version() -> Result<(), Box<dyn std::error::Error>> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;

    let readme = std::fs::read_to_string(workspace_root.join("README.md"))?;
    assert!(
        readme.contains(&format!("workspace version `{VERSION}`")),
        "README.md should report workspace version `{VERSION}`"
    );

    let changelog = std::fs::read_to_string(workspace_root.join("CHANGELOG.md"))?;
    assert_eq!(
        changelog.lines().next().unwrap_or_default(),
        format!("# {VERSION}"),
        "CHANGELOG.md first heading should match the hya package version"
    );

    let manifest = std::fs::read_to_string(workspace_root.join("Cargo.toml"))?;
    assert!(
        manifest.contains(&format!("[workspace.package]\nversion = \"{VERSION}\"")),
        "Cargo.toml [workspace.package] version should match the hya package version"
    );

    let tui_manifest =
        std::fs::read_to_string(workspace_root.join("packages/hya-tui-ts/package.json"))?;
    assert!(
        tui_manifest.contains(&format!("\"version\": \"{VERSION}\"")),
        "packaged TypeScript TUI version should match the hya package version"
    );

    let lockfile = std::fs::read_to_string(workspace_root.join("Cargo.lock"))?;
    let mismatched = hya_lockfile_version_mismatches(&lockfile);
    assert!(
        mismatched.is_empty(),
        "Cargo.lock hya package versions should all be {VERSION}: {mismatched:?}"
    );

    Ok(())
}

fn hya_lockfile_version_mismatches(lockfile: &str) -> Vec<String> {
    lockfile
        .split("[[package]]")
        .filter_map(|package| {
            let name = lockfile_field(package, "name")?;
            let version = lockfile_field(package, "version")?;
            if name == "hya" || name.starts_with("hya-") {
                Some((name, version))
            } else {
                None
            }
        })
        .filter_map(|(name, version)| {
            if version == VERSION {
                None
            } else {
                Some(format!("{name}={version}"))
            }
        })
        .collect()
}

fn lockfile_field<'a>(package: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field} = \"");
    package
        .lines()
        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('"'))
}
