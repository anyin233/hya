//! Guardrail: the workspace must not retain a Rust interactive TUI crate.
//! The only interactive frontend is `packages/hya-tui-ts`.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should resolve")
}

#[test]
fn retained_rust_tui_crates_are_removed() {
    let root = workspace_root();
    for rel in ["crates/hya-tui", "crates/hya-tui-lib", "crates/hya-parity"] {
        let path = root.join(rel);
        assert!(
            !path.exists(),
            "{rel} must not exist; TypeScript TUI is the sole frontend"
        );
    }
}

#[test]
fn cargo_manifests_do_not_depend_on_rust_tui() {
    let root = workspace_root();
    let root_manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .expect("root Cargo.toml should be readable");
    assert!(
        !root_manifest.contains("hya-tui-lib"),
        "root Cargo.toml must not pin hya-tui-lib"
    );
    assert!(
        !root_manifest.contains("hya-tui"),
        "root Cargo.toml must not reference hya-tui"
    );

    let crates_dir = root.join("crates");
    let mut offenders = Vec::new();
    walk_cargo_tomls(&crates_dir, &mut |path, body| {
        // Path deps and package names that reintroduce a Rust TUI.
        for needle in [
            "name = \"hya-tui\"",
            "name = \"hya-tui-lib\"",
            "name = \"hya-parity\"",
            "hya-tui =",
            "hya-tui-lib =",
            "hya-parity =",
            "path = \"../hya-tui\"",
            "path = \"../hya-tui-lib\"",
            "path = \"crates/hya-tui\"",
            "path = \"crates/hya-tui-lib\"",
        ] {
            if body.contains(needle) {
                offenders.push(format!("{}: {needle}", path.display()));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "crate manifests must not reintroduce Rust TUI packages: {offenders:?}"
    );
}

fn walk_cargo_tomls(dir: &Path, visit: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip vendored / huge trees if any appear under crates later.
            if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            walk_cargo_tomls(&path, visit);
            continue;
        }
        if path.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml") {
            if let Ok(body) = std::fs::read_to_string(&path) {
                visit(&path, &body);
            }
        }
    }
}
