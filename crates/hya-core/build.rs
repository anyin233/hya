//! Build-time preparation for the trusted embedded core-agents preset.

use std::fmt::Write as _;
use std::path::PathBuf;

use hya_bundle::{
    AgentRole, BundleSource, PreparedInstallableBundle, SpawnLifecycle, prepare_package,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreAgentsPolicy {
    reserved_ids: Vec<String>,
    ordinary_spawn_scope: String,
}

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let preset_dir = manifest_dir.join("../../bundles/presets/core-agents");
    println!("cargo:rerun-if-changed={}", preset_dir.display());

    let source = BundleSource::read_directory(&preset_dir)
        .unwrap_or_else(|error| panic!("failed to read core-agents preset: {error}"));
    let prepared = prepare_package(source)
        .unwrap_or_else(|error| panic!("failed to prepare core-agents preset: {error}"));
    let bundle = prepared
        .bundles()
        .first()
        .and_then(PreparedInstallableBundle::agent_set_bundle)
        .unwrap_or_else(|| panic!("core-agents preset must contain one AgentSetBundle"));
    if prepared.bundles().len() != 1 || bundle.identity.id != "hya/core-agents" {
        panic!("core-agents preset must contain exactly hya/core-agents");
    }
    let policy_resource = bundle
        .extensions
        .iter()
        .find(|resource| resource.local_id == "policy")
        .unwrap_or_else(|| panic!("core-agents preset must contain extensions.files policy"));
    let mut policy: CoreAgentsPolicy = serde_norway::from_str(&policy_resource.content)
        .unwrap_or_else(|error| panic!("invalid core-agents policy: {error}"));
    policy.reserved_ids.sort();
    policy.reserved_ids.dedup();
    if policy.ordinary_spawn_scope != "all_ordinary" {
        panic!("core-agents ordinary_spawn_scope must be all_ordinary");
    }
    for reserved in &policy.reserved_ids {
        if !bundle
            .agents
            .iter()
            .any(|agent| agent.id.as_str() == reserved)
        {
            panic!("core-agents policy reserves unknown agent `{reserved}`");
        }
    }

    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").unwrap_or_else(|| panic!("Cargo did not set OUT_DIR")),
    );
    std::fs::write(out_dir.join("core-agents.prepared.json"), prepared.bytes())
        .unwrap_or_else(|error| panic!("failed to write prepared core-agents bytes: {error}"));
    std::fs::write(out_dir.join("core-agents.digest"), prepared.digest())
        .unwrap_or_else(|error| panic!("failed to write core-agents digest: {error}"));

    // Compatibility source is generated from the prepared preset. Runtime catalog
    // assembly reads the embedded prepared document directly.
    let mut generated = String::from(
        "/// Compatibility roster generated from the embedded core-agents preset.\n\
         pub const BUILTIN_AGENTS: &[BuiltinAgent] = &[\n",
    );
    for agent in &bundle.agents {
        let role = match agent.role {
            AgentRole::Main => "hya_bundle::AgentRole::Main",
            AgentRole::Subagent => "hya_bundle::AgentRole::Subagent",
        };
        let lifecycle = match agent.spawn_lifecycle {
            SpawnLifecycle::Transient => "hya_bundle::SpawnLifecycle::Transient",
            SpawnLifecycle::Resident => "hya_bundle::SpawnLifecycle::Resident",
        };
        let reserved = policy
            .reserved_ids
            .binary_search_by(|reserved| reserved.as_str().cmp(agent.id.as_str()))
            .is_ok();
        let scope = if reserved {
            "SpawnScope::None"
        } else {
            "SpawnScope::AllOrdinary"
        };
        writeln!(
            generated,
            "BuiltinAgent {{ id: {:?}, description: {:?}, role: {role}, prompt: {:?}, model_policy: BuiltinModelPolicy {{ model: {:?}, category: {:?}, reasoning: {:?} }}, spawn_lifecycle: {lifecycle}, spawn_scope: {scope}, system_reserved: {reserved} }},",
            agent.id.as_str(),
            agent.description.as_deref(),
            agent.prompt.as_deref(),
            agent.model_policy.model.as_deref(),
            agent.model_policy.category.as_deref(),
            agent.model_policy.reasoning.as_deref(),
        )
        .unwrap_or_else(|error| panic!("failed to generate core-agents shim: {error}"));
    }
    generated.push_str("];\n");
    writeln!(
        generated,
        "/// Engine-only ids declared by the embedded preset policy.\n\
         pub const CORE_AGENT_RESERVED_IDS: &[&str] = &{:?};",
        policy.reserved_ids
    )
    .unwrap_or_else(|error| panic!("failed to generate reserved agent policy: {error}"));
    std::fs::write(out_dir.join("core_agents_roster.rs"), generated)
        .unwrap_or_else(|error| panic!("failed to write core-agents shim: {error}"));
}
