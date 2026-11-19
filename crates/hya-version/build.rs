use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let rustc = Command::new(env::var_os("RUSTC").expect("cargo sets RUSTC"))
        .arg("-V")
        .output()
        .expect("run rustc -V");
    let rustc = String::from_utf8(rustc.stdout).expect("rustc -V prints UTF-8");
    println!("cargo:rustc-env=HYA_RUSTC={}", rustc.trim());
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let versions_path = manifest_dir.join("../../versions.toml");
    println!("cargo:rerun-if-changed={}", versions_path.display());
    let source = fs::read_to_string(&versions_path).expect("read versions.toml");
    for (key, env_name) in [
        ("version", "HYA_VERSION"),
        ("minimum_backend_version", "HYA_MINIMUM_BACKEND_VERSION"),
    ] {
        let value = top_level_value(&source, key)
            .unwrap_or_else(|| panic!("versions.toml is missing `{key}`"));
        println!("cargo:rustc-env={env_name}={value}");
    }
}

fn top_level_value<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            break;
        }
        let (candidate, value) = line.split_once('=')?;
        if candidate.trim() == key {
            return value.trim().strip_prefix('"')?.strip_suffix('"');
        }
    }
    None
}
