//! Prepare and validate the embedded tool-family Plugin policies.
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use hya_bundle::{BundleSource, PreparedBundleKind, prepare_package};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: u32,
    identity: String,
    protected_names: Vec<String>,
    #[serde(default)]
    schemes: Vec<Scheme>,
    tools: Vec<Tool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scheme {
    scheme: String,
    tool: String,
    #[serde(default)]
    writable: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    name: String,
    schema_version: u32,
    permission: Permission,
    #[serde(default = "default_true")]
    exposed: bool,
    #[serde(default)]
    aliases: Vec<Alias>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Permission {
    ReadOnly,
    Task,
    Tool,
    Command,
    Mcp,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alias {
    name: String,
    visibility: Visibility,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreSkillFrontmatter {
    name: String,
    description: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Visibility {
    Hidden,
    Public,
}

const fn default_true() -> bool {
    true
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let families = [
        ("base-tools", "hya/base-tools"),
        ("extended-tools", "hya/extended-tools"),
        ("network-tools", "hya/network-tools"),
        ("channel-tools", "hya/channel-tools"),
        ("todo-tools", "hya/todo-tools"),
    ];
    let mut policies = Vec::with_capacity(families.len());
    for (directory, identity) in families {
        let preset_dir = manifest_dir.join("../../bundles/presets").join(directory);
        println!("cargo:rerun-if-changed={}", preset_dir.display());
        let source = BundleSource::read_directory(&preset_dir).expect("read tool preset source");
        let prepared = prepare_package(source).expect("prepare tool preset Plugin");
        let [bundle] = prepared.bundles() else {
            panic!("tool preset must prepare one bundle")
        };
        assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
        assert_eq!(bundle.identity().id, identity);
        let asset = bundle
            .extensions()
            .iter()
            .find(|asset| asset.local_id == "exposure")
            .expect("tool preset must contain exposure policy");
        let policy: Policy =
            serde_norway::from_str(&asset.content).expect("parse prepared exposure policy");
        validate(&policy, identity);
        policies.push((
            policy,
            bundle.digest().to_string(),
            prepared.bytes().to_vec(),
        ));
    }
    validate_global(&policies);
    let generated = generate(&policies);
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("base_tools_preset.rs");
    fs::write(output, generated).expect("write generated tool-family policies");
    prepare_core_skills(&manifest_dir);
}

fn prepare_core_skills(manifest_dir: &std::path::Path) {
    let preset_dir = manifest_dir.join("../../bundles/presets/core-skills");
    println!("cargo:rerun-if-changed={}", preset_dir.display());
    let source = BundleSource::read_directory(&preset_dir).expect("read core skills source");
    let prepared = prepare_package(source).expect("prepare core skills Plugin");
    let [bundle] = prepared.bundles() else {
        panic!("core skills preset must prepare one bundle")
    };
    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert_eq!(bundle.identity().id, "hya/core-skills");
    let resources = bundle.skills();
    assert_eq!(resources.len(), 2, "core skills preset must own two Skills");
    let mut output = String::from("static CORE_SKILL_ROWS: &[(&str, &str, &str)] = &[\n");
    for (resource, expected) in resources
        .iter()
        .zip(["agent-bundle-authoring", "secure-self-update"])
    {
        assert_eq!(resource.local_id, expected);
        let rest = resource
            .content
            .strip_prefix("---\n")
            .expect("core Skill frontmatter start");
        let (frontmatter, body) = rest
            .split_once("\n---\n")
            .expect("core Skill frontmatter end");
        let parsed: CoreSkillFrontmatter =
            serde_norway::from_str(frontmatter).expect("parse core Skill frontmatter");
        assert_eq!(parsed.name, resource.local_id);
        assert!(!parsed.description.trim().is_empty());
        writeln!(
            output,
            "({:?}, {:?}, {:?}),",
            parsed.name, parsed.description, body
        )
        .expect("write core Skill row");
    }
    writeln!(
        output,
        "]; static CORE_SKILLS_PREPARED_BYTES: &[u8] = &{:?};",
        prepared.bytes()
    )
    .expect("write core skills prepared bytes");
    let path =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("core_skills_preset.rs");
    fs::write(path, output).expect("write generated core Skill metadata");
}

fn validate(policy: &Policy, expected_identity: &str) {
    assert_eq!(policy.schema_version, 1);
    assert_eq!(policy.identity, expected_identity);
    let names = policy
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), policy.tools.len(), "duplicate canonical tool");
    let mut exports = names.clone();
    for tool in &policy.tools {
        assert!(
            tool.schema_version > 0,
            "zero schema version for {}",
            tool.name
        );
        for alias in &tool.aliases {
            assert!(
                exports.insert(&alias.name),
                "duplicate alias {}",
                alias.name
            );
        }
    }
    for protected in &policy.protected_names {
        assert!(
            names.contains(protected.as_str()),
            "protected name is not canonical: {protected}"
        );
    }
    for scheme in &policy.schemes {
        assert!(
            names.contains(scheme.tool.as_str()),
            "scheme tool is not canonical: {}",
            scheme.tool
        );
    }
}

fn validate_global(policies: &[(Policy, String, Vec<u8>)]) {
    let mut exports = BTreeSet::new();
    for (policy, _, _) in policies {
        for tool in &policy.tools {
            assert!(
                exports.insert(tool.name.as_str()),
                "duplicate tool across families"
            );
            for alias in &tool.aliases {
                assert!(
                    exports.insert(alias.name.as_str()),
                    "duplicate alias across families"
                );
            }
        }
    }
}

fn generate(policies: &[(Policy, String, Vec<u8>)]) -> String {
    let mut out = String::new();
    for (family, (policy, _, bytes)) in policies.iter().enumerate() {
        for (index, tool) in policy.tools.iter().enumerate() {
            writeln!(
                out,
                "static ALIASES_{family}_{index}: &[BaseToolAlias] = &["
            )
            .expect("write aliases");
            for alias in &tool.aliases {
                let visibility = match alias.visibility {
                    Visibility::Hidden => "Hidden",
                    Visibility::Public => "Public",
                };
                writeln!(
                    out,
                    "BaseToolAlias {{ name: {:?}, visibility: AliasVisibility::{visibility} }},",
                    alias.name
                )
                .expect("write alias");
            }
            writeln!(out, "];").expect("close aliases");
        }
        writeln!(out, "static TOOLS_{family}: &[BaseToolExposure] = &[").expect("write tools");
        for (index, tool) in policy.tools.iter().enumerate() {
            let permission = match tool.permission {
                Permission::ReadOnly => "ReadOnly",
                Permission::Task => "Task",
                Permission::Tool => "Tool",
                Permission::Command => "Command",
                Permission::Mcp => "Mcp",
            };
            writeln!(out, "BaseToolExposure {{ name: {:?}, schema_version: {}, permission: ToolPermission::{permission}, exposed: {}, aliases: ALIASES_{family}_{index} }},", tool.name, tool.schema_version, tool.exposed).expect("write tool");
        }
        writeln!(out, "];").expect("close tools");
        writeln!(out, "static SCHEMES_{family}: &[BaseToolScheme] = &[").expect("write schemes");
        for scheme in &policy.schemes {
            writeln!(
                out,
                "BaseToolScheme {{ scheme: {:?}, tool: {:?}, writable: {} }},",
                scheme.scheme, scheme.tool, scheme.writable
            )
            .expect("write scheme");
        }
        writeln!(
            out,
            "]; static PROTECTED_NAMES_{family}: &[&str] = &{:?};",
            policy.protected_names
        )
        .expect("write protected");
        writeln!(out, "static PREPARED_BYTES_{family}: &[u8] = &{:?};", bytes)
            .expect("write bytes");
    }
    writeln!(
        out,
        "pub(super) static TOOL_BUNDLE_PRESETS: &[BaseToolsPreset] = &["
    )
    .expect("open presets");
    for (family, (policy, digest, _)) in policies.iter().enumerate() {
        writeln!(out, "BaseToolsPreset {{ schema_version: {}, identity: {:?}, bundle_digest: {:?}, prepared_catalog_bytes: PREPARED_BYTES_{family}, protected_names: PROTECTED_NAMES_{family}, schemes: SCHEMES_{family}, tools: TOOLS_{family} }},", policy.schema_version, policy.identity, digest).expect("write preset");
    }
    writeln!(out, "];").expect("close presets");
    out
}
