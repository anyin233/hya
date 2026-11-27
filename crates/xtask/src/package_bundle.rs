//! Deterministic public `.hyabundle` packaging.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

#[derive(Deserialize)]
struct ToolDescriptorHeader {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tools: Vec<NativeTool>,
}

/// Validate one source directory and atomically write canonical public package bytes.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let [source, output] = args.as_slice() else {
        bail!("usage: cargo run -p xtask -- package-bundle <source-directory> <output.hyabundle>");
    };
    let source = BundleSource::read_directory(source)
        .with_context(|| format!("read bundle source directory {source}"))?;
    write_package(source, PathBuf::from(output))
}

/// Add a target-specific Rust dynamic library to a native bundle source.
pub fn run_native_library(args: Vec<String>) -> anyhow::Result<()> {
    let [source, library, output] = args.as_slice() else {
        bail!(
            "usage: cargo run -p xtask -- package-native-tool-library <source-directory> <built-library> <output.hyabundle>"
        );
    };
    package_native_library(source.as_ref(), library.as_ref(), output.as_ref())
}

/// Package a native bundle source with its built dynamic library at `output`.
///
/// Supports both standardized `runtime: { kind: native }` manifests (with
/// structured `tools/*.{json,yaml}` descriptors and no `native/src/*.rs`
/// entries in `bundle.yaml`) and legacy `exposure.yaml` + `extensions:`
/// manifests.
pub(crate) fn package_native_library(
    source: &Path,
    library: &Path,
    output: &Path,
) -> anyhow::Result<()> {
    let source_root = source;
    let manifest_path = source_root.join("bundle.yaml");
    let exposure_path = source_root.join("exposure.yaml");
    let mut manifest: Value = serde_norway::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    )
    .context("parse tool-family bundle manifest")?;
    // A tool family may declare its tools in `exposure.yaml`, in `resources.tools`
    // via structured `tools/` descriptors, or both; an internal bundle without
    // tools (e.g. compaction) has none.
    let exposure_tools = if exposure_path.is_file() {
        let policy: NativePolicy = serde_norway::from_slice(
            &std::fs::read(&exposure_path)
                .with_context(|| format!("read {}", exposure_path.display()))?,
        )
        .context("parse tool-family exposure policy")?;
        if policy.tools.is_empty() {
            bail!("native tool-family policy has no tools");
        }
        policy.tools
    } else {
        Vec::new()
    };
    let root = manifest
        .as_object_mut()
        .context("bundle manifest must be a mapping")?;
    if root.get("kind").and_then(Value::as_str) != Some("Bundle") {
        bail!("native bundle source must be a Bundle");
    }
    let filename = library
        .file_name()
        .and_then(|name| name.to_str())
        .context("native library needs a UTF-8 file name")?;
    let library_path = format!("native/{filename}");

    if let Some(runtime_val) = root.get_mut("runtime") {
        let runtime = runtime_val
            .as_object_mut()
            .context("bundle runtime must be a mapping")?;
        if runtime.get("kind").and_then(Value::as_str) != Some("native") {
            bail!("native library packaging requires `runtime.kind: native`");
        }
        if runtime.get("library").is_some_and(|value| !value.is_null()) {
            bail!("native bundle source already declares `runtime.library`");
        }
        runtime.insert("library".into(), json!(library_path));
    } else if root.contains_key("extensions") {
        bail!("tool-family source uses legacy `extensions`; migrate to `schema_version: 2` and `runtime`");
    } else {
        root.insert(
            "runtime".into(),
            json!({"kind": "native", "library": library_path}),
        );
    }

    let mut source = BundleSource::read_directory(source_root)
        .with_context(|| format!("read bundle source directory {}", source_root.display()))?;

    let existing_tools = root
        .get("resources")
        .and_then(|res| res.get("tools"))
        .and_then(Value::as_array)
        .filter(|arr| !arr.is_empty());

    if let Some(declared_tools) = existing_tools {
        if !exposure_tools.is_empty() {
            let declared_ids: BTreeSet<&str> = declared_tools
                .iter()
                .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                .collect();
            let exposure_ids: BTreeSet<&str> = exposure_tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect();
            if declared_ids != exposure_ids {
                bail!(
                    "manifest resources.tools ({declared_ids:?}) does not match exposure.yaml ({exposure_ids:?})"
                );
            }
        }
    } else if !exposure_tools.is_empty() {
        let mut needs_placeholder = false;
        let mut entries = Vec::with_capacity(exposure_tools.len());
        for tool in exposure_tools {
            if let Some(descriptor_path) = find_structured_tool_descriptor(source_root, &tool.name)
            {
                entries.push(json!({"id": tool.name, "path": descriptor_path}));
            } else {
                needs_placeholder = true;
                entries.push(json!({"id": tool.name, "path": "declarations/tool.json"}));
            }
        }
        let resources = root
            .entry("resources")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("resources must be a mapping")?;
        resources.insert("tools".into(), Value::Array(entries));
        if needs_placeholder {
            source = source.with_file("declarations/tool.json", b"{}".to_vec());
        }
    } else {
        let discovered = discover_structured_tools(source_root)?;
        if !discovered.is_empty() {
            let resources = root
                .entry("resources")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .context("resources must be a mapping")?;
            resources.insert(
                "tools".into(),
                Value::Array(
                    discovered
                        .into_iter()
                        .map(|(id, path)| json!({"id": id, "path": path}))
                        .collect(),
                ),
            );
        }
    }

    let source = source
        .with_file(
            "bundle.yaml",
            serde_norway::to_string(&manifest)?.into_bytes(),
        )
        .with_file(
            library_path,
            std::fs::read(library)
                .with_context(|| format!("read built native library {}", library.display()))?,
        );
    write_package(source, output.to_path_buf())
}

/// Look for a structured tool descriptor under `tools/` matching `tool_name`.
fn find_structured_tool_descriptor(source_root: &Path, tool_name: &str) -> Option<String> {
    for ext in ["json", "yaml", "yml"] {
        let rel = format!("tools/{tool_name}.{ext}");
        if source_root.join(&rel).is_file() {
            return Some(rel);
        }
    }
    for rel in ["tools/tools.json", "tools/tools.yaml", "tools/tools.yml"] {
        let full = source_root.join(rel);
        if full.is_file()
            && let Ok(bytes) = std::fs::read(&full)
            && let Ok(header) = parse_tool_descriptor_header(rel, &bytes)
            && header.tools.iter().any(|tool| tool.name == tool_name)
        {
            return Some(rel.to_string());
        }
    }
    None
}

/// Discover structured tool descriptors under `source_root/tools/` when no
/// `exposure.yaml` or `resources.tools` is present.
fn discover_structured_tools(source_root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let tools_dir = source_root.join("tools");
    if !tools_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = std::fs::read_dir(&tools_dir)
        .with_context(|| format!("read {}", tools_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("list {}", tools_dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".json") && !name.ends_with(".yaml") && !name.ends_with(".yml") {
            continue;
        }
        let rel = format!("tools/{name}");
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let header = parse_tool_descriptor_header(&rel, &bytes)
            .with_context(|| format!("parse tool descriptor {}", path.display()))?;
        if !header.tools.is_empty() {
            for tool in header.tools {
                found.push((tool.name, rel.clone()));
            }
        } else if let Some(tool_name) = header.name {
            found.push((tool_name, rel));
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(found)
}

fn parse_tool_descriptor_header(rel: &str, bytes: &[u8]) -> anyhow::Result<ToolDescriptorHeader> {
    if rel.ends_with(".json") {
        serde_json::from_slice(bytes).context("parse JSON tool descriptor")
    } else {
        serde_norway::from_slice(bytes).context("parse YAML tool descriptor")
    }
}

/// Package one bundle source directory at `output`.
pub(crate) fn package_directory(source: &Path, output: &Path) -> anyhow::Result<()> {
    let bundle = BundleSource::read_directory(source)
        .with_context(|| format!("read bundle source directory {}", source.display()))?;
    write_package(bundle, output.to_path_buf())
}

fn write_package(source: BundleSource, output: PathBuf) -> anyhow::Result<()> {
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
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

    use hya_bundle::{PreparedRuntimeKind, inspect_public_package};

    use super::*;

    #[test]
    fn native_tool_library_package_has_no_process_and_keeps_binary_bytes() {
        let root = std::env::temp_dir().join(format!(
            "hya-native-library-package-test-{}",
            std::process::id()
        ));
        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("bundle.yaml"), "schema_version: 2\nkind: Bundle\nidentity: { id: hya/network-tools, version: 1.0.0, publisher: hya }\nnamespace: network-tools\nruntime:\n  kind: native\nassets: [{ id: exposure, path: exposure.yaml }]\n").unwrap();
        std::fs::write(source.join("exposure.yaml"), "schema_version: 1\nidentity: hya/network-tools\nprotected_names: []\ntools:\n  - { name: webfetch, schema_version: 1, permission: read_only }\n").unwrap();
        let library = root.join(format!(
            "libhya_network_tools{}",
            std::env::consts::DLL_SUFFIX
        ));
        let bytes = [0xcf, 0xfa, 0xed, 0xfe, 0, 0xff];
        std::fs::write(&library, bytes).unwrap();
        let package = root.join("network.hyabundle");
        run_native_library(vec![
            source.to_string_lossy().into_owned(),
            library.to_string_lossy().into_owned(),
            package.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let catalog = inspect_public_package(&std::fs::read(&package).unwrap()).unwrap();
        assert!(catalog.process_extensions().is_empty());
        let [bundle] = catalog.bundles() else {
            panic!("one bundle")
        };
        assert_eq!(bundle.tools()[0].local_id, "webfetch");
        let resource = bundle
            .extensions()
            .iter()
            .find(|item| item.local_id == "runtime")
            .unwrap();
        assert!(resource.stable_id.contains("/library/runtime"));
        assert_eq!(resource.source_bytes().unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn standardized_native_bundle_packages_structured_tools_without_native_sources() {
        let root =
            std::env::temp_dir().join(format!("hya-native-v2-package-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        std::fs::create_dir_all(source.join("tools")).unwrap();
        std::fs::create_dir_all(source.join("native/src")).unwrap();
        std::fs::write(
            source.join("bundle.yaml"),
            "kind: Bundle\nschema_version: 2\nidentity: { id: acme/net, version: 1.0.0, publisher: acme }\nruntime:\n  kind: native\n  abi_version: 1\n  crate_dir: native\n  capabilities: [tools]\nresources:\n  tools:\n    - { id: webfetch, path: tools/webfetch.json }\n",
        )
        .unwrap();
        std::fs::write(
            source.join("tools/webfetch.json"),
            r#"{"schema_version":1,"name":"webfetch","description":"Fetch a URL.","permission":"tool","entrypoint":{"native":"webfetch"}}"#,
        )
        .unwrap();
        // Rust source files exist on disk, NOT declared in bundle.yaml.
        std::fs::write(
            source.join("native/Cargo.toml"),
            "[package]\nname=\"net\"\n",
        )
        .unwrap();
        std::fs::write(source.join("native/src/lib.rs"), "pub fn register() {}\n").unwrap();

        let library = root.join(format!("libacme_net{}", std::env::consts::DLL_SUFFIX));
        let bytes = [0x7f, b'E', b'L', b'F', 2, 1];
        std::fs::write(&library, bytes).unwrap();
        let package = root.join("net.hyabundle");

        package_native_library(&source, &library, &package).unwrap();
        let catalog = inspect_public_package(&std::fs::read(&package).unwrap()).unwrap();
        let [bundle] = catalog.bundles() else {
            panic!("one bundle");
        };
        assert_eq!(bundle.runtime().unwrap().kind, PreparedRuntimeKind::Native);
        assert_eq!(bundle.tools().len(), 1);
        assert_eq!(bundle.tools()[0].source_path, "tools/webfetch.json");
        assert!(
            !bundle
                .extensions()
                .iter()
                .any(|item| item.source_path.ends_with(".rs"))
        );
        let lib = bundle
            .extensions()
            .iter()
            .find(|item| item.stable_id.contains("/library/runtime"))
            .unwrap();
        assert_eq!(lib.source_bytes().unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exposure_yaml_prefers_structured_tool_descriptors_when_present() {
        let root = std::env::temp_dir().join(format!(
            "hya-native-hybrid-package-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("source");
        std::fs::create_dir_all(source.join("tools")).unwrap();
        std::fs::write(
            source.join("bundle.yaml"),
            "schema_version: 2\nkind: Bundle\nidentity: { id: hya/channel-tools, version: 1.0.0, publisher: hya }\nruntime:\n  kind: native\nassets: [{ id: exposure, path: exposure.yaml }]\n",
        )
        .unwrap();
        std::fs::write(
            source.join("exposure.yaml"),
            "schema_version: 1\nidentity: hya/channel-tools\nprotected_names: []\ntools:\n  - { name: fetch, schema_version: 1, permission: read_only }\n",
        )
        .unwrap();
        std::fs::write(
            source.join("tools/fetch.json"),
            r#"{"schema_version":1,"name":"fetch","description":"Fetch web content.","permission":"read_only"}"#,
        )
        .unwrap();
        let library = root.join(format!("libhya_hybrid{}", std::env::consts::DLL_SUFFIX));
        std::fs::write(&library, [1, 2, 3, 4]).unwrap();
        let package = root.join("hybrid.hyabundle");

        package_native_library(&source, &library, &package).unwrap();
        let catalog = inspect_public_package(&std::fs::read(&package).unwrap()).unwrap();
        let [bundle] = catalog.bundles() else {
            panic!("one bundle");
        };
        assert_eq!(bundle.tools()[0].local_id, "fetch");
        assert_eq!(bundle.tools()[0].source_path, "tools/fetch.json");
        std::fs::remove_dir_all(root).unwrap();
    }
}
