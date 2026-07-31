use std::error::Error;
use std::path::PathBuf;

use hya_bundle::{BundleSource, prepare_builtins};

fn main() -> Result<(), Box<dyn Error>> {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let repository = crate_dir
        .parent()
        .and_then(|crates| crates.parent())
        .ok_or("hya-app must live under <repository>/crates")?;
    let core = repository.join("bundles/builtin/hya-core-agents");
    let development = repository.join("bundles/builtin/hya-development");
    println!("cargo:rerun-if-changed={}", core.display());
    println!("cargo:rerun-if-changed={}", development.display());

    let prepared = prepare_builtins(vec![
        BundleSource::read_directory(core)?,
        BundleSource::read_directory(development)?,
    ])?;
    let output = PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(output.join("builtin-bundles.json"), prepared.bytes())?;
    std::fs::write(
        output.join("builtin-bundles.sha256"),
        prepared.digest().as_bytes(),
    )?;
    Ok(())
}
