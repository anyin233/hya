//! Deterministic public `.hyabundle` packaging.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use hya_bundle::{BundleSource, write_public_package};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct NativePolicy {
    tools: Vec<NativeTool>,
}

#[derive(Deserialize)]
struct NativeTool {
    name: String,
}

/// Validate one source directory and atomically write canonical public package bytes.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let [source, output] = args.as_slice() else {
        bail!("usage: cargo xtask package-bundle <source-directory> <output.hyabundle>");
    };
    let source = BundleSource::read_directory(source)
        .with_context(|| format!("read bundle source directory {source}"))?;
    write_package(source, PathBuf::from(output))
}

/// Add a target-specific Rust executable to a policy-only tool-family source.
pub fn run_native(args: Vec<String>) -> anyhow::Result<()> {
    let [source, binary, output] = args.as_slice() else {
        bail!(
            "usage: cargo xtask package-native-tool-bundle <source-directory> <built-executable> <output.hyabundle>"
        );
    };
    let source_root = PathBuf::from(source);
    let manifest_path = source_root.join("bundle.yaml");
    let exposure_path = source_root.join("exposure.yaml");
    let mut manifest: Value = serde_norway::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    )
    .context("parse tool-family bundle manifest")?;
    let policy: NativePolicy = serde_norway::from_slice(
        &std::fs::read(&exposure_path)
            .with_context(|| format!("read {}", exposure_path.display()))?,
    )
    .context("parse tool-family exposure policy")?;
    if policy.tools.is_empty() {
        bail!("native tool-family policy has no tools");
    }
    let root = manifest
        .as_object_mut()
        .context("bundle manifest must be a mapping")?;
    if root.get("kind").and_then(Value::as_str) != Some("Plugin") {
        bail!("native tool-family source must be a Plugin");
    }
    let extensions = root
        .get_mut("extensions")
        .and_then(Value::as_object_mut)
        .context("tool-family source needs extensions.files")?;
    if extensions.contains_key("rust") || extensions.contains_key("process") {
        bail!("tool-family source already declares a native process");
    }
    extensions.insert(
        "rust".into(),
        json!([{"id":"runtime","path":"native/tool-runtime"}]),
    );
    extensions.insert(
        "process".into(),
        json!({"kind":"rust","command":["${BUNDLE_ROOT}/native/tool-runtime"]}),
    );
    let resources = root.entry("resources").or_insert_with(|| json!({}));
    let resources = resources
        .as_object_mut()
        .context("resources must be a mapping")?;
    if resources.contains_key("tools") {
        bail!("tool-family source already declares tool resources");
    }
    resources.insert(
        "tools".into(),
        Value::Array(
            policy
                .tools
                .into_iter()
                .map(|tool| json!({"id":tool.name,"path":"declarations/tool.json"}))
                .collect(),
        ),
    );
    let source = BundleSource::read_directory(&source_root)
        .with_context(|| format!("read bundle source directory {source}"))?
        .with_file(
            "bundle.yaml",
            serde_norway::to_string(&manifest)?.into_bytes(),
        )
        .with_file("declarations/tool.json", b"{}".to_vec())
        .with_file(
            "native/tool-runtime",
            std::fs::read(binary).with_context(|| format!("read built executable {binary}"))?,
        );
    write_package(source, PathBuf::from(output))
}

fn write_package(source: BundleSource, output: PathBuf) -> anyhow::Result<()> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create package output directory {}", parent.display()))?;

    let bytes = write_public_package(&source).context("write deterministic public bundle")?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bundle.hyabundle"),
        std::process::id()
    ));
    std::fs::write(&temporary, bytes)
        .with_context(|| format!("write temporary package {}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &output) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("activate package output {}", output.display()));
    }
    Ok(())
}

#[cfg(test)]
mod native_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use hya_bundle::inspect_public_package;

    use super::*;

    #[test]
    fn native_tool_package_carries_binary_and_exact_family_declarations() {
        let root =
            std::env::temp_dir().join(format!("hya-native-package-test-{}", std::process::id()));
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("bundle.yaml"), "kind: Plugin\nidentity: { id: hya/test-tools, version: 1.0.0, publisher: hya }\nnamespace: test-tools\nextensions:\n  files: [{ id: exposure, path: exposure.yaml }]\n").unwrap();
        std::fs::write(source.join("exposure.yaml"), "schema_version: 1\nidentity: hya/test-tools\nprotected_names: []\ntools:\n  - { name: alpha, schema_version: 1, permission: read_only }\n  - { name: beta, schema_version: 1, permission: tool }\n").unwrap();
        let binary = root.join("native-tool");
        std::fs::write(&binary, [0xff, 0x00, 0x7f, 0x45, 0x4c, 0x46]).unwrap();
        let package = root.join("native.hyabundle");
        run_native(vec![
            source.to_string_lossy().into_owned(),
            binary.to_string_lossy().into_owned(),
            package.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let catalog = inspect_public_package(&std::fs::read(&package).unwrap()).unwrap();
        let [bundle] = catalog.bundles() else {
            panic!("one bundle")
        };
        assert_eq!(
            bundle
                .tools()
                .iter()
                .map(|tool| tool.local_id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        let runtime = bundle
            .extensions()
            .iter()
            .find(|asset| asset.local_id == "runtime")
            .unwrap();
        assert_eq!(
            runtime.source_bytes().unwrap(),
            [0xff, 0x00, 0x7f, 0x45, 0x4c, 0x46]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
