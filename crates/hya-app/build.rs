//! Build-time preparation of the immutable first-party WorkflowBundle.

use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use hya_bundle::{BundleSource, prepare_package};

/// Prepare the immutable first-party WorkflowBundle for runtime embedding.
fn main() {
    if let Err(error) = prepare_first_party_bundle() {
        panic!("prepare first-party WorkflowBundle: {error}");
    }
}

/// Read, validate, and emit the canonical first-party prepared catalog bytes.
fn prepare_first_party_bundle() -> Result<(), Box<dyn Error>> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or("CARGO_MANIFEST_DIR is not available to the hya-app build")?;
    let source_root = Path::new(&manifest_dir)
        .join("../../bundles/first-party/plan-impl-review")
        .canonicalize()?;
    println!(
        "cargo:rerun-if-changed={}",
        source_root
            .parent()
            .and_then(Path::parent)
            .ok_or("first-party bundle parent is unavailable")?
            .display()
    );

    let source = BundleSource::read_directory(&source_root)?;
    let prepared = prepare_package(source)?;
    let out_dir = env::var_os("OUT_DIR").ok_or("OUT_DIR is not available to the hya-app build")?;
    let out_dir = Path::new(&out_dir);
    fs::write(out_dir.join("first-party.prepared.json"), prepared.bytes())?;
    fs::write(
        out_dir.join("first-party.prepared.digest"),
        prepared.digest(),
    )?;
    Ok(())
}
