//! Build-time preparation of the immutable first-party bundles.

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

/// Read, validate, and emit the canonical prepared bytes for every
/// first-party bundle directory (sorted by directory name for determinism).
fn prepare_first_party_bundle() -> Result<(), Box<dyn Error>> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or("CARGO_MANIFEST_DIR is not available to the hya-app build")?;
    let first_party_root = Path::new(&manifest_dir).join("../../bundles/first-party");
    let mut dirs = Vec::new();
    for entry in fs::read_dir(&first_party_root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            dirs.push(name.to_string());
        }
    }
    dirs.sort();
    let out_dir = env::var_os("OUT_DIR").ok_or("OUT_DIR is not available to the hya-app build")?;
    let out_dir = Path::new(&out_dir);
    let mut entry_parts: Vec<String> = Vec::new();
    for name in &dirs {
        let source_root = first_party_root.join(name).canonicalize()?;
        println!("cargo:rerun-if-changed={}", source_root.display());
        let source = BundleSource::read_directory(&source_root)?;
        let prepared = prepare_package(source)?;
        entry_parts.push(format!(
            "{{\"name\":\"{name}\",\"digest\":\"{}\",\"bytes\":{} }}",
            prepared.digest(),
            serde_json_lenient(prepared.bytes())
        ));
    }
    let combined = format!("[{}]", entry_parts.join(","));
    fs::write(out_dir.join("first-party.json"), combined)?;
    Ok(())
}

/// Minimal JSON string encoder for the canonical UTF-8 prepared bytes.
fn serde_json_lenient(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
