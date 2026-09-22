//! Prepare and validate the embedded `hya/base-tools` Plugin policy.
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
    let preset_dir = manifest_dir.join("../../bundles/presets/base-tools");
    println!(
        "cargo:rerun-if-changed={}",
        preset_dir.join("bundle.yaml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        preset_dir.join("exposure.yaml").display()
    );
    let source = BundleSource::read_directory(&preset_dir).expect("read hya/base-tools source");
    let prepared = prepare_package(source).expect("prepare hya/base-tools Plugin");
    let [bundle] = prepared.bundles() else {
        panic!("hya/base-tools must prepare one bundle")
    };
    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert_eq!(bundle.identity().id, "hya/base-tools");
    let [asset] = bundle.extensions() else {
        panic!("hya/base-tools must contain one policy asset")
    };
    assert_eq!(asset.local_id, "exposure");
    let policy: Policy =
        serde_norway::from_str(&asset.content).expect("parse prepared exposure policy");
    validate(&policy);
    let generated = generate(&policy, bundle.digest(), prepared.bytes());
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("base_tools_preset.rs");
    fs::write(output, generated).expect("write generated base-tools policy");
}

fn validate(policy: &Policy) {
    assert_eq!(policy.schema_version, 1);
    assert_eq!(policy.identity, "hya/base-tools");
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

fn generate(policy: &Policy, digest: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    for (index, tool) in policy.tools.iter().enumerate() {
        writeln!(out, "static ALIASES_{index}: &[BaseToolAlias] = &[").expect("write aliases");
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
    writeln!(out, "static TOOLS: &[BaseToolExposure] = &[").expect("write tools");
    for (index, tool) in policy.tools.iter().enumerate() {
        let permission = match tool.permission {
            Permission::ReadOnly => "ReadOnly",
            Permission::Task => "Task",
            Permission::Tool => "Tool",
            Permission::Command => "Command",
            Permission::Mcp => "Mcp",
        };
        writeln!(out, "BaseToolExposure {{ name: {:?}, schema_version: {}, permission: ToolPermission::{permission}, exposed: {}, aliases: ALIASES_{index} }},", tool.name, tool.schema_version, tool.exposed).expect("write tool");
    }
    writeln!(out, "];").expect("close tools");
    writeln!(out, "static SCHEMES: &[BaseToolScheme] = &[").expect("write schemes");
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
        "]; static PROTECTED_NAMES: &[&str] = &{:?};",
        policy.protected_names
    )
    .expect("write protected");
    writeln!(out, "static PREPARED_BYTES: &[u8] = &{:?};", bytes).expect("write bytes");
    writeln!(out, "pub(super) static BASE_TOOLS_PRESET: BaseToolsPreset = BaseToolsPreset {{ schema_version: {}, identity: {:?}, bundle_digest: {:?}, prepared_catalog_bytes: PREPARED_BYTES, protected_names: PROTECTED_NAMES, schemes: SCHEMES, tools: TOOLS }};", policy.schema_version, policy.identity, digest).expect("write preset");
    out
}
