#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::error::BundleError;
use crate::first_party::{
    BuiltinBundleProfile, BuiltinBundleSelection, BuiltinBundleStatus, FIRST_PARTY_BUNDLES,
    resolve_first_party_bundles_with,
};
use crate::model::{
    AgentRole, PreparedJsRuntimeMode, PreparedNativeCapability, PreparedProcessKind,
    PreparedRuntimeKind,
};
use crate::package::{inspect_public_package, write_public_package};
use crate::prepare::prepare_package;
use crate::source::{BundleSource, SourceFile};
use crate::{BundleCatalog, ExportKind, PreparedCatalog};

#[test]
fn standardized_v2_js_process_bundle_prepares_and_round_trips() -> Result<(), BundleError> {
    let source = BundleSource::new(
        "v2-js",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity:
  id: acme/v2-js
  version: 1.0.0
  publisher: acme
runtime:
  kind: js
  mode: process
  entry: src/index.ts
  command: [bun, run, '${BUNDLE_ROOT}/src/index.ts']
  timeout_ms: 15000
resources:
  tools:
    - id: search
      path: tools/search.json
  hooks:
    - id: tool.execute.before
      path: hooks/hooks.json
  mcp:
    - id: remote-docs
      path: mcp/remote.json
  commands:
    - id: review
      path: commands/review.md
agents:
  - path: agents/scout.md
apis:
  - id: status
    method: GET
    scope: global
    path: /status
    response_schema: schemas/status.json
assets:
  - id: status-schema
    path: schemas/status.json
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"console.log('ok');\n".as_slice()),
            SourceFile::new(
                "tools/search.json",
                br#"{
  "schema_version": 1,
  "name": "search",
  "description": "Search indexed documents.",
  "permission": "read_only",
  "aliases": [{ "name": "find_docs", "visibility": "hidden" }],
  "parameters": { "type": "object", "properties": { "q": { "type": "string" } }, "required": ["q"] },
  "entrypoint": { "js": "src/index.ts#search" }
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{
  "schema_version": 1,
  "hooks": [
    {
      "event": "tool.execute.before",
      "matcher": "^bash$",
      "handler": { "kind": "rpc", "method": "hook/tool.execute.before" },
      "failure_mode": "fail_closed",
      "timeout_ms": 5000
    }
  ]
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "mcp/remote.json",
                br#"{
  "transport": "http",
  "url": "https://mcp.example.com/v1",
  "headers": { "Authorization": "Bearer ${env:EXAMPLE_TOKEN}" },
  "timeout_ms": 30000
}"#
                .as_slice(),
            ),
            SourceFile::new(
                "commands/review.md",
                b"---\nname: review\ndescription: Review changes\nhints: [diff]\nsubtask: false\n---\nReview $ARGUMENTS\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "agents/scout.md",
                b"---\nid: v2-scout\nrole: subagent\ndescription: Fast scout\nresource_view:\n  allow: [search, remote-docs]\n---\nYou are a fast scout.\n"
                    .as_slice(),
            ),
            SourceFile::new("schemas/status.json", br#"{"type":"object"}"#.as_slice()),
            // Undeclared dev test file must be ignored by deterministic pack.
            SourceFile::new("src/index.test.ts", b"test('x', () => {});\n".as_slice()),
        ],
    );

    let prepared = prepare_package(source.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("expected single bundle");
    };
    let runtime = bundle.runtime().expect("runtime preserved");
    assert_eq!(runtime.kind, PreparedRuntimeKind::Js);
    assert_eq!(runtime.mode, Some(PreparedJsRuntimeMode::Process));
    assert_eq!(runtime.entry.as_deref(), Some("src/index.ts"));

    let process = prepared
        .bundle_process("acme/v2-js")
        .expect("process derived from runtime");
    assert_eq!(process.kind, PreparedProcessKind::Bun);
    assert_eq!(process.timeout_ms, Some(15_000));

    assert_eq!(bundle.commands().len(), 1);
    assert_eq!(bundle.commands()[0].local_id, "review");
    assert_eq!(
        bundle.commands()[0].stable_id,
        "bundle:acme/v2-js/command/review"
    );

    assert_eq!(bundle.agents().len(), 1);
    assert_eq!(bundle.agents()[0].id.as_str(), "v2-scout");
    assert_eq!(bundle.agents()[0].role, AgentRole::Subagent);
    assert_eq!(
        bundle.agents()[0].prompt_source.as_deref(),
        Some("agents/scout.md")
    );
    assert_eq!(
        bundle.agents()[0].prompt.as_deref(),
        Some("You are a fast scout.")
    );

    let catalog = BundleCatalog::from_verified_catalogs(&[&prepared])?;
    assert!(
        catalog
            .resolve_resource("acme/v2-js", ExportKind::Command, "review")
            .is_ok()
    );

    // Deterministic archive includes commands, assets, entry, descriptors, and agent .md,
    // while omitting undeclared test files.
    let archive_a = write_public_package(&source)?;
    let archive_b = write_public_package(&source)?;
    assert_eq!(archive_a, archive_b);
    let inspected = inspect_public_package(&archive_a)?;
    assert_eq!(inspected.digest(), prepared.digest());
    Ok(())
}

#[test]
fn native_bundle_does_not_require_declaring_native_rust_sources() -> Result<(), BundleError> {
    let source = BundleSource::new(
        "v2-native",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity:
  id: acme/native-tools
  version: 1.0.0
  publisher: acme
runtime:
  kind: native
  abi_version: 1
  crate_dir: native
  library: native/libacme_native_tools.so
  capabilities: [tools, hooks]
resources:
  tools:
    - id: fetch
      path: tools/fetch.yaml
  hooks:
    - id: session.start
      path: hooks/hooks.json
"#
                .as_slice(),
            ),
            SourceFile::new(
                "tools/fetch.yaml",
                b"schema_version: 1\nname: fetch\ndescription: Fetch URL\npermission: read_only\nentrypoint:\n  native: fetch\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{"schema_version":1,"hooks":[{"event":"session.start","handler":{"kind":"native","target":"on_start"}}]}"#
                    .as_slice(),
            ),
            // Source Rust files exist on disk, NOT listed in bundle.yaml.
            SourceFile::new("native/Cargo.toml", b"[package]\nname = \"x\"\n".as_slice()),
            SourceFile::new("native/src/lib.rs", b"pub fn x() {}\n".as_slice()),
            SourceFile::new(
                "native/libacme_native_tools.so",
                vec![0x7f, b'E', b'L', b'F', 0, 1],
            ),
        ],
    );

    let prepared = prepare_package(source.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("expected single bundle");
    };
    let runtime = bundle.runtime().expect("native runtime");
    assert_eq!(runtime.kind, PreparedRuntimeKind::Native);
    assert_eq!(runtime.abi_version, Some(1));
    assert_eq!(
        runtime.capabilities,
        vec![
            PreparedNativeCapability::Tools,
            PreparedNativeCapability::Hooks,
        ]
    );
    let lib = bundle
        .extensions()
        .iter()
        .find(|res| res.stable_id == "bundle:acme/native-tools/library/runtime")
        .expect("packaged library resource");
    assert_eq!(lib.source_path, "native/libacme_native_tools.so");
    assert_eq!(
        lib.source_bytes().expect("decode binary"),
        vec![0x7f, b'E', b'L', b'F', 0, 1]
    );

    // Packing strips native/src/lib.rs and native/Cargo.toml cleanly.
    let bytes = write_public_package(&source)?;
    let inspected = inspect_public_package(&bytes)?;
    assert_eq!(inspected.digest(), prepared.digest());
    Ok(())
}

#[test]
fn rejects_legacy_extensions_missing_or_v1_schema_and_invalid_descriptors() {
    // 1. Removed `extensions:` block (process / libraries / files / js / rust) rejected with schema_version: 2
    let legacy_extensions = BundleSource::new(
        "legacy-extensions",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Bundle
schema_version: 2
identity: { id: acme/conflict, version: 1.0.0, publisher: acme }
runtime: { kind: static }
extensions:
  process: { kind: bun, command: [bun, run, index.ts] }
"#
            .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(legacy_extensions),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("unknown field `extensions`")
    ));

    // 2. Missing schema_version and schema_version: 1 both rejected
    let missing_schema = BundleSource::new(
        "missing-schema",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nidentity: { id: acme/missing, version: 1.0.0, publisher: acme }\n"
                .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(missing_schema),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("schema_version: 2")
    ));

    let v1_schema = BundleSource::new(
        "v1-schema",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nschema_version: 1\nidentity: { id: acme/v1, version: 1.0.0, publisher: acme }\n"
                .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(v1_schema),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("schema_version `1`")
    ));

    // 3. Unsupported schema_version
    let bad_schema = BundleSource::new(
        "bad-schema",
        vec![SourceFile::new(
            "bundle.yaml",
            b"kind: Bundle\nschema_version: 99\nidentity: { id: acme/bad, version: 1.0.0, publisher: acme }\n"
                .as_slice(),
        )],
    );
    assert!(matches!(
        prepare_package(bad_schema),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("schema_version")
    ));

    // 4. tool.execute.before with fail_open rejected
    let bad_hook = BundleSource::new(
        "bad-hook",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/bad-hook, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: process, command: [bun, run, index.ts] }
resources:
  hooks: [{ id: tool.execute.before, path: hooks/hooks.json }]
assets: [{ id: index, path: index.ts }]
"#
                .as_slice(),
            ),
            SourceFile::new("index.ts", b"export {}\n".as_slice()),
            SourceFile::new(
                "hooks/hooks.json",
                br#"{"schema_version":1,"hooks":[{"event":"tool.execute.before","handler":{"kind":"rpc","method":"hook/tool.execute.before"},"failure_mode":"fail_open"}]}"#
                    .as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_hook),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("fail_closed")
    ));

    // 5. MCP declaring both command and url rejected
    let bad_mcp = BundleSource::new(
        "bad-mcp",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/bad-mcp, version: 1.0.0, publisher: acme }\nresources:\n  mcp: [{ id: both, path: mcp/both.json }]\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "mcp/both.json",
                br#"{"command":["node","srv.js"],"url":"https://example.com"}"#.as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_mcp),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("not both")
    ));

    // 6. Tool descriptor name mismatch rejected
    let bad_tool = BundleSource::new(
        "bad-tool",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/bad-tool, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: managed, entry: src/index.ts }
resources:
  tools: [{ id: search, path: tools/search.json }]
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"export {}\n".as_slice()),
            SourceFile::new(
                "tools/search.json",
                br#"{"schema_version":1,"name":"other","description":"Mismatch"}"#.as_slice(),
            ),
        ],
    );
    assert!(matches!(
        prepare_package(bad_tool),
        Err(BundleError::InvalidManifest { detail, .. }) if detail.contains("declares name")
    ));
}

#[test]
fn rejects_exposure_yaml_and_exposure_asset_except_transitional_tool_families()
-> Result<(), BundleError> {
    let exposure_yaml = b"schema_version: 1\nidentity: acme/bad\nprotected_names: []\ntools: []\n";

    // 1. Undeclared `exposure.yaml` at root rejected.
    let undeclared = BundleSource::new(
        "undeclared-exposure",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/bad, version: 1.0.0, publisher: acme }\n"
                    .as_slice(),
            ),
            SourceFile::new("exposure.yaml", exposure_yaml.as_slice()),
        ],
    );
    assert!(matches!(
        prepare_package(undeclared),
        Err(BundleError::InvalidManifest { detail, .. })
            if detail.contains("exposure.yaml") && detail.contains("schema_version 2")
    ));

    // 2. Undeclared nested `sub/exposure.yaml` rejected.
    let nested = BundleSource::new(
        "nested-exposure",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/bad-nested, version: 1.0.0, publisher: acme }\n"
                    .as_slice(),
            ),
            SourceFile::new("sub/exposure.yaml", exposure_yaml.as_slice()),
        ],
    );
    assert!(matches!(
        prepare_package(nested),
        Err(BundleError::InvalidManifest { detail, .. })
            if detail.contains("exposure.yaml") && detail.contains("schema_version 2")
    ));

    // 3. `exposure.yaml` declared under `assets:` rejected (with `id: exposure` or any id).
    for asset_id in ["exposure", "legacy-policy"] {
        let yaml = format!(
            "kind: Bundle\nschema_version: 2\nidentity: {{ id: acme/bad-asset-{asset_id}, version: 1.0.0, publisher: acme }}\nassets:\n  - {{ id: {asset_id}, path: exposure.yaml }}\n"
        );
        let declared = BundleSource::new(
            "declared-exposure",
            vec![
                SourceFile::new("bundle.yaml", yaml.into_bytes()),
                SourceFile::new("exposure.yaml", exposure_yaml.as_slice()),
            ],
        );
        assert!(matches!(
            prepare_package(declared),
            Err(BundleError::InvalidManifest { detail, .. })
                if detail.contains("exposure.yaml") && detail.contains("schema_version 2")
        ));
    }

    // 4. `assets:` declaring `id: exposure` or alias `exposure` on another filename rejected.
    let by_id = BundleSource::new(
        "exposure-asset-id",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/bad-id, version: 1.0.0, publisher: acme }\nassets:\n  - { id: exposure, path: policy.yaml }\n"
                    .as_slice(),
            ),
            SourceFile::new("policy.yaml", exposure_yaml.as_slice()),
        ],
    );
    assert!(matches!(
        prepare_package(by_id),
        Err(BundleError::InvalidManifest { detail, .. })
            if detail.contains("exposure") && detail.contains("schema_version 2")
    ));

    let by_alias = BundleSource::new(
        "exposure-asset-alias",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: acme/bad-alias, version: 1.0.0, publisher: acme }\nassets:\n  - { id: policy, path: policy.yaml, aliases: [exposure] }\n"
                    .as_slice(),
            ),
            SourceFile::new("policy.yaml", exposure_yaml.as_slice()),
        ],
    );
    assert!(matches!(
        prepare_package(by_alias),
        Err(BundleError::InvalidManifest { detail, .. })
            if detail.contains("exposure") && detail.contains("schema_version 2")
    ));

    // 5. The four transitional Phase 0–4 legacy tool-family bundles remain accepted.
    for transitional_id in [
        "hya/base-tools",
        "hya/extended-tools",
        "hya/network-tools",
        "hya/channel-tools",
    ] {
        let manifest = format!(
            "kind: Bundle\nschema_version: 2\nidentity: {{ id: {transitional_id}, version: 1.0.0, publisher: hya }}\nassets:\n  - {{ id: exposure, path: exposure.yaml }}\n"
        );
        let policy = format!(
            "schema_version: 1\nidentity: {transitional_id}\nprotected_names: []\ntools: []\n"
        );
        let ok = prepare_package(BundleSource::new(
            transitional_id,
            vec![
                SourceFile::new("bundle.yaml", manifest.into_bytes()),
                SourceFile::new("exposure.yaml", policy.into_bytes()),
            ],
        ))?;
        assert_eq!(ok.bundles()[0].identity().id, transitional_id);
    }
    Ok(())
}

#[test]
fn supports_managed_js_static_runtime_claude_agent_md_and_multi_tool_yaml()
-> Result<(), BundleError> {
    // 1. Managed JS runtime + multi-tool tools.yaml + standalone agent with Claude Code `name:` & default role.
    let managed = BundleSource::new(
        "managed-js",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/managed, version: 1.0.0, publisher: acme }
runtime:
  kind: js
  mode: managed
  entry: src/index.ts
resources:
  tools:
    - { id: alpha, path: tools/tools.yaml }
    - { id: beta, path: tools/tools.yaml }
agents:
  - path: agents/helper.md
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"export const alpha = () => {};\n".as_slice()),
            SourceFile::new(
                "tools/tools.yaml",
                b"schema_version: 1\ntools:\n  - name: alpha\n    description: Alpha tool\n    permission: read_only\n  - name: beta\n    description: Beta tool\n    permission: tool\n"
                    .as_slice(),
            ),
            SourceFile::new(
                "agents/helper.md",
                b"---\nname: claude-helper\ndescription: Claude-compatible agent\n---\nAssist the user.\n"
                    .as_slice(),
            ),
        ],
    );
    let prepared = prepare_package(managed.clone())?;
    let [bundle] = prepared.bundles() else {
        panic!("one bundle");
    };
    assert_eq!(
        bundle.runtime().unwrap().mode,
        Some(PreparedJsRuntimeMode::Managed)
    );
    assert!(prepared.bundle_process("acme/managed").is_none());
    assert_eq!(bundle.tools().len(), 2);
    assert_eq!(bundle.agents()[0].id.as_str(), "claude-helper");
    assert_eq!(bundle.agents()[0].role, AgentRole::Subagent);
    let packed = write_public_package(&managed)?;
    assert_eq!(inspect_public_package(&packed)?.digest(), prepared.digest());

    // 2. Explicit static runtime + stem-derived agent id.
    let static_bundle = BundleSource::new(
        "static-rt",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/static-rt, version: 1.0.0, publisher: acme }
runtime:
  kind: static
agents:
  - path: agents/reviewer.md
"#
                .as_slice(),
            ),
            SourceFile::new(
                "agents/reviewer.md",
                b"---\ndescription: Code reviewer\n---\nReview carefully.\n".as_slice(),
            ),
        ],
    );
    let static_prep = prepare_package(static_bundle)?;
    assert_eq!(
        static_prep.bundles()[0].runtime().unwrap().kind,
        PreparedRuntimeKind::Static
    );
    assert_eq!(static_prep.bundles()[0].agents()[0].id.as_str(), "reviewer");
    Ok(())
}

#[test]
fn supports_overrides_hya_bare_and_strictly_reserves_todo_and_rewrite() -> Result<(), BundleError> {
    // 1. Valid manifest-level and tool-level `overrides: hya/bare` on non-reserved tools (`read`).
    let manifest_override = BundleSource::new(
        "override-bare",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/custom-read, version: 1.0.0, publisher: acme }
overrides: hya/bare
runtime: { kind: js, mode: managed, entry: src/index.ts }
resources:
  tools:
    - { id: read, path: tools/read.json }
"#
                .as_slice(),
            ),
            SourceFile::new(
                "src/index.ts",
                b"export const read = () => {};\n".as_slice(),
            ),
            SourceFile::new(
                "tools/read.json",
                br#"{"schema_version":1,"name":"read","description":"Custom read"}"#.as_slice(),
            ),
        ],
    );
    let prepared = prepare_package(manifest_override)?;
    let bundle = &prepared.bundles()[0];
    assert_eq!(bundle.overrides(), Some("hya/bare"));
    let decoded = PreparedCatalog::decode(prepared.bytes(), prepared.digest())?;
    assert_eq!(decoded.bundles()[0].overrides(), Some("hya/bare"));

    // 2. Tool-level and descriptor-level `overrides: hya/bare`.
    let tool_override = BundleSource::new(
        "tool-override-bare",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/custom-bash, version: 1.0.0, publisher: acme }
runtime: { kind: js, mode: managed, entry: src/index.ts }
resources:
  tools:
    - { id: bash, path: tools/bash.json, overrides: hya/bare }
    - { id: grep, path: tools/grep.json }
"#
                .as_slice(),
            ),
            SourceFile::new("src/index.ts", b"export {}\n".as_slice()),
            SourceFile::new(
                "tools/bash.json",
                br#"{"schema_version":1,"name":"bash","description":"Custom bash"}"#.as_slice(),
            ),
            SourceFile::new(
                "tools/grep.json",
                br#"{"schema_version":1,"name":"grep","description":"Custom grep","overrides":"hya/bare"}"#
                    .as_slice(),
            ),
        ],
    );
    let tool_prep = prepare_package(tool_override)?;
    assert_eq!(tool_prep.bundles()[0].overrides(), None);
    assert_eq!(
        tool_prep.bundles()[0].tools()[0].overrides.as_deref(),
        Some("hya/bare")
    );
    assert_eq!(
        tool_prep.bundles()[0].tools()[1].overrides.as_deref(),
        Some("hya/bare")
    );

    // 3. Reject overriding reserved `todo` and `rewrite` (manifest-level, tool-level, or via alias).
    for (reserved_id, aliases) in [
        ("todo", "[]"),
        ("rewrite", "[]"),
        ("custom", "[\"todo\"]"),
        ("custom", "[\"rewrite\"]"),
    ] {
        let yaml = format!(
            "kind: Bundle\nschema_version: 2\nidentity: {{ id: acme/bad-{reserved_id}, version: 1.0.0, publisher: acme }}\noverrides: hya/bare\nruntime: {{ kind: js, mode: managed, entry: src/index.ts }}\nresources:\n  tools:\n    - {{ id: {reserved_id}, path: tools/tool.json, aliases: {aliases} }}\n"
        );
        let desc = format!(
            "{{\"schema_version\":1,\"name\":\"{reserved_id}\",\"description\":\"Forbidden\"}}"
        );
        let bad = BundleSource::new(
            "reserved-override",
            vec![
                SourceFile::new("bundle.yaml", yaml.into_bytes()),
                SourceFile::new("src/index.ts", b"export {}\n".as_slice()),
                SourceFile::new("tools/tool.json", desc.into_bytes()),
            ],
        );
        assert!(
            matches!(
                prepare_package(bad),
                Err(BundleError::ReservedToolOverride { .. })
            ),
            "expected ReservedToolOverride for id={reserved_id} aliases={aliases}"
        );
    }

    // 4. Reject `overrides` on non-tool resources (skills).
    let non_tool = BundleSource::new(
        "non-tool-override",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: acme/bad-skill, version: 1.0.0, publisher: acme }
resources:
  skills:
    - { id: helper, path: skills/helper.md, overrides: hya/bare }
"#
                .as_slice(),
            ),
            SourceFile::new("skills/helper.md", b"# Helper\n".as_slice()),
        ],
    );
    assert!(matches!(
        prepare_package(non_tool),
        Err(BundleError::InvalidOverride { .. })
    ));
    Ok(())
}

#[test]
fn builtin_bundle_profile_and_selective_resolution_never_panics() -> Result<(), BundleError> {
    let bare = BuiltinBundleSelection {
        profile: BuiltinBundleProfile::Bare,
        enable: vec![
            "hya/base-tools".to_string(),
            "hya/core-skills".to_string(),
            "unknown/missing".to_string(),
        ],
        disable: vec!["hya/core-skills".to_string()],
    };
    assert!(bare.is_enabled("hya/base-tools"));
    assert!(!bare.is_enabled("hya/core-skills")); // disable overrides enable
    assert!(!bare.is_enabled("hya/network-tools"));
    assert_eq!(bare.enabled_identities(), vec!["hya/base-tools"]);
    assert_eq!(bare.unknown_identities(), vec!["unknown/missing"]);

    // Full profile with one override bundle and one disabled bundle and one missing bundle.
    let project_override = prepare_package(BundleSource::new(
        "project-override",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Bundle
schema_version: 2
identity: { id: acme/custom-skills, version: 1.0.0, publisher: acme }
overrides: hya/core-skills
"#
            .as_slice(),
        )],
    ))?;
    let sample_catalog = prepare_package(BundleSource::new(
        "sample-base",
        vec![SourceFile::new(
            "bundle.yaml",
            br#"kind: Bundle
schema_version: 2
identity: { id: hya/base-tools, version: 1.0.0, publisher: hya }
"#
            .as_slice(),
        )],
    ))?;
    let full = BuiltinBundleSelection {
        profile: BuiltinBundleProfile::Full,
        enable: Vec::new(),
        disable: vec!["hya/network-tools".to_string()],
    };
    let report =
        resolve_first_party_bundles_with(&full, &[&project_override.bundles()[0]], |id| match id {
            "hya/base-tools" => Ok(Some(sample_catalog.clone())),
            "hya/extended-tools" => Err(BundleError::InvalidManifest {
                source_name: id.to_string(),
                detail: "corrupt".to_string(),
            }),
            _ => Ok(None),
        });
    assert_eq!(report.entries.len(), FIRST_PARTY_BUNDLES.len());
    assert_eq!(
        report.status("hya/base-tools"),
        Some(&BuiltinBundleStatus::Loaded)
    );
    assert_eq!(
        report.status("hya/network-tools"),
        Some(&BuiltinBundleStatus::Disabled)
    );
    assert_eq!(
        report.status("hya/core-skills"),
        Some(&BuiltinBundleStatus::Overridden {
            by: "acme/custom-skills".to_string()
        })
    );
    assert!(matches!(
        report.status("hya/extended-tools"),
        Some(BuiltinBundleStatus::Failed { .. })
    ));
    assert!(matches!(
        report.status("hya/channel-tools"),
        Some(BuiltinBundleStatus::Missing { .. })
    ));
    assert_eq!(report.loaded.len(), 1);
    Ok(())
}
