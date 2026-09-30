use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let versions_path = manifest_dir.join("../../versions.toml");
    println!("cargo:rerun-if-changed={}", versions_path.display());
    let source = fs::read_to_string(&versions_path).expect("read versions.toml");
    for (section, env_name) in [
        ("backend", "HYA_BACKEND_VERSION"),
        ("frontend", "HYA_FRONTEND_VERSION"),
    ] {
        let version = section_version(&source, section)
            .unwrap_or_else(|| panic!("versions.toml is missing [{section}].version"));
        println!("cargo:rustc-env={env_name}={version}");
    }
}

fn section_version<'a>(source: &'a str, section: &str) -> Option<&'a str> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == header;
            continue;
        }
        if in_section {
            let (key, value) = line.split_once('=')?;
            if key.trim() == "version" {
                return value.trim().strip_prefix('"')?.strip_suffix('"');
            }
        }
    }
    None
}
