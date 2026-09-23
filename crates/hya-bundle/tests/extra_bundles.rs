//! `hya-extra/*` distribution bundles: every source directory under
//! `bundles/extra/` must prepare cleanly, follow the extras identity rules
//! (`hya-extra/<name>` id, `hya-extra` publisher, workspace version), and
//! package deterministically through the canonical writer.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_bundle::{
    AgentRole, BundleSource, PreparedBundleKind, first_party_source_root, inspect_public_package,
    prepare_package, write_public_package,
};

fn extra_bundle_dirs() -> Vec<PathBuf> {
    let root = first_party_source_root().join("extra");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_extra_bundle_prepares_and_packages_deterministically() {
    let dirs = extra_bundle_dirs();
    assert!(
        !dirs.is_empty(),
        "expected at least one bundles/extra/* source directory"
    );

    for dir in &dirs {
        let source = BundleSource::read_directory(dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));

        let prepared = prepare_package(source.clone())
            .unwrap_or_else(|error| panic!("prepare {}: {error}", dir.display()));
        assert_eq!(
            prepared.bundles().len(),
            1,
            "expected exactly one bundle in {}",
            dir.display()
        );
        let bundle = &prepared.bundles()[0];
        let identity = bundle.identity();
        assert!(
            identity.id.starts_with("hya-extra/"),
            "{}: identity id {} must start with hya-extra/",
            dir.display(),
            identity.id
        );
        assert_eq!(
            identity.publisher,
            "hya-extra",
            "{}: publisher must be hya-extra",
            dir.display()
        );
        assert_eq!(
            identity.version,
            env!("CARGO_PKG_VERSION"),
            "{}: extras version with hya (identity version must equal workspace version)",
            dir.display()
        );

        // Canonical package writer output is deterministic: packaging the same
        // source twice must produce byte-identical archives.
        let first = write_public_package(&source)
            .unwrap_or_else(|error| panic!("package {}: {error}", dir.display()));
        let second = write_public_package(&source)
            .unwrap_or_else(|error| panic!("package {}: {error}", dir.display()));
        assert_eq!(
            first,
            second,
            "non-deterministic package bytes for {}",
            dir.display()
        );

        // Re-preparing from the packaged archive reaches the same digest as
        // preparing directly from the source directory.
        let reprepared = inspect_public_package(&first)
            .unwrap_or_else(|error| panic!("inspect packaged {}: {error}", dir.display()));
        assert_eq!(
            reprepared.digest(),
            prepared.digest(),
            "archive round-trip digest mismatch for {}",
            dir.display()
        );
    }
}

#[test]
fn zvec_grep_is_a_plugin_with_one_mcp_server_and_one_skill() {
    let dir = first_party_source_root().join("extra/zvec-grep");
    let source = BundleSource::read_directory(&dir).expect("read zvec-grep source");
    let prepared = prepare_package(source).expect("prepare zvec-grep");
    let bundle = &prepared.bundles()[0];

    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert!(
        bundle.agents().is_empty(),
        "a Plugin bundle carries no synthetic agent"
    );
    assert_eq!(bundle.mcp().len(), 1, "expected exactly one MCP server");
    assert_eq!(bundle.skills().len(), 1, "expected exactly one Skill");
    assert_eq!(bundle.mcp()[0].local_id, "zvec-grep");
    assert_eq!(bundle.skills()[0].local_id, "zvec-grep");
}

#[test]
fn scout_is_an_agent_set_bundle_with_a_subagent_scout_and_its_own_mcp_server() {
    let dir = first_party_source_root().join("extra/scout");
    let source = BundleSource::read_directory(&dir).expect("read scout source");
    let prepared = prepare_package(source).expect("prepare scout");
    let bundle = &prepared.bundles()[0];

    assert_eq!(bundle.kind(), PreparedBundleKind::AgentSetBundle);
    assert_eq!(bundle.agents().len(), 1, "expected exactly one agent");
    let agent = &bundle.agents()[0];
    assert_eq!(agent.id.as_str(), "scout");
    assert_eq!(agent.role, AgentRole::Subagent);
    assert_eq!(
        bundle.mcp().len(),
        1,
        "scout ships its own MCP server (bundle agents cannot see a sibling Plugin's resources)"
    );
    // The prepared resource view resolved successfully (prepare_package fails
    // closed on any unresolved reference), and selects at least the harness
    // read/grep/glob tools plus the bundle-local MCP server.
    assert!(
        agent.resource_view.allow.len() >= 4,
        "expected read/grep/glob plus the mcp server in scout's resource_view.allow: {:?}",
        agent.resource_view.allow
    );
}

#[test]
fn jev_model_router_is_a_bun_process_plugin_with_one_chat_params_hook() {
    let dir = first_party_source_root().join("extra/jev-model-router");
    let source = BundleSource::read_directory(&dir).expect("read jev-model-router source");
    let prepared = prepare_package(source).expect("prepare jev-model-router");
    let bundle = &prepared.bundles()[0];

    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert_eq!(bundle.namespace(), "jev-model-router");
    assert!(bundle.agents().is_empty() && bundle.tools().is_empty());
    let hooks: Vec<_> = bundle
        .hooks()
        .iter()
        .map(|hook| hook.local_id.as_str())
        .collect();
    assert_eq!(hooks, ["chat.params"], "exactly one chat.params hook");
    let process = prepared
        .bundle_process("hya-extra/jev-model-router")
        .expect("explicit extensions.process");
    assert_eq!(process.kind, hya_bundle::PreparedProcessKind::Bun);
    assert_eq!(
        process.command,
        ["bun", "run", "${BUNDLE_ROOT}/router.ts"],
        "router.ts speaks the plugin protocol itself (no adapter is injected)"
    );
    let files: Vec<_> = bundle
        .extensions()
        .iter()
        .map(|file| file.source_path.as_str())
        .collect();
    assert_eq!(
        files,
        ["router.ts"],
        "tests, README and examples stay unpackaged"
    );
}

#[test]
fn model_fallback_is_a_bun_process_plugin_with_one_model_fallback_hook() {
    let dir = first_party_source_root().join("extra/model-fallback");
    let source = BundleSource::read_directory(&dir).expect("read model-fallback source");
    let prepared = prepare_package(source).expect("prepare model-fallback");
    let bundle = &prepared.bundles()[0];

    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert_eq!(bundle.namespace(), "model-fallback");
    assert!(bundle.agents().is_empty() && bundle.tools().is_empty());
    let hooks: Vec<_> = bundle
        .hooks()
        .iter()
        .map(|hook| hook.local_id.as_str())
        .collect();
    assert_eq!(hooks, ["model.fallback"], "exactly one model.fallback hook");
    let process = prepared
        .bundle_process("hya-extra/model-fallback")
        .expect("explicit extensions.process");
    assert_eq!(process.kind, hya_bundle::PreparedProcessKind::Bun);
    assert_eq!(
        process.command,
        ["bun", "run", "${BUNDLE_ROOT}/fallback.ts"],
        "fallback.ts speaks the plugin protocol itself (no adapter is injected)"
    );
    let files: Vec<_> = bundle
        .extensions()
        .iter()
        .map(|file| file.source_path.as_str())
        .collect();
    assert_eq!(files, ["fallback.ts"], "tests stay unpackaged (undeclared)");
}
