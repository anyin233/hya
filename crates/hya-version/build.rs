use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let versions_path = manifest_dir.join("../../versions.toml");
    println!("cargo:rerun-if-changed={}", versions_path.display());
    let source = fs::read_to_string(&versions_path).expect("read versions.toml");
    for (section, key, env_name) in [
        ("backend", "version", "HYA_BACKEND_VERSION"),
        ("frontend", "version", "HYA_FRONTEND_VERSION"),
        (
            "frontend",
            "minimum_backend_version",
            "HYA_MINIMUM_BACKEND_VERSION",
        ),
    ] {
        let value = section_value(&source, section, key)
            .unwrap_or_else(|| panic!("versions.toml is missing [{section}].{key}"));
        println!("cargo:rustc-env={env_name}={value}");
    }
}

fn section_value<'a>(source: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if in_section {
            let (candidate, value) = line.split_once('=')?;
            if candidate.trim() == key {
                return value.trim().strip_prefix('"')?.strip_suffix('"');
            }
        }
    }
    None
}
