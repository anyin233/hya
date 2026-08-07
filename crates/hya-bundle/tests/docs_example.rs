//! Prove the repository documentation example is prepare-valid through the
//! production v1 Markdown preparer.

use std::path::{Path, PathBuf};

use hya_bundle::{
    AgentRole, BundleSource, PreparedCatalog, SourceFile, SpawnLifecycle,
    prepare_package,
};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("hya-bundle must live under <repository>/crates"))
}

fn docs_example_path() -> PathBuf {
    repository_root().join("docs/examples/bundle.hya.md")
}

#[test]
fn docs_example_bundle_hya_md_prepares_deterministically() {
    // Given: the repository documentation example named exactly bundle.hya.md.
    let path = docs_example_path();
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("docs example must exist at {}: {error}", path.display()));
    assert!(
        !bytes.is_empty(),
        "docs example at {} must be nonempty preparer source",
        path.display()
    );

    // When: the example is passed as a single SourceFile through the production preparer.
    let first = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes.clone())],
    ));
    let first = first.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully: {error:?}");
    });
    let second = prepare_package(BundleSource::new(
        "docs-example",
        vec![SourceFile::new("bundle.hya.md", bytes)],
    ));
    let second = second.unwrap_or_else(|error| {
        panic!("docs example must prepare successfully on second pass: {error:?}");
    });

    // Then: preparation succeeds with one flat main/transient agent and is deterministic.
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.bundles().len(), 1);
    assert_eq!(first.index().len(), 1);

    let bundle = &first.bundles()[0];
    let agent = &bundle.agent;
    assert_eq!(agent.role, AgentRole::Main);
    assert_eq!(agent.spawn_lifecycle, SpawnLifecycle::Transient);
    assert!(
        agent
            .prompt
            .as_deref()
            .is_some_and(|prompt| !prompt.trim().is_empty()),
        "markdown body must become a nonempty prepared prompt"
    );
    assert_eq!(agent.prompt_source.as_deref(), Some("bundle.hya.md"));
}

#[test]
fn bundle_authoring_commands_enumerate_regular_closure_files() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("crates/hya-server/src/compat/skill_templates/agent-bundle-authoring.md"),
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "bundle authoring documentation must exist at {}: {error}",
                path.display()
            )
        });
        assert!(
            !source.contains("bundle.hya.md tools extensions"),
            "{} must not pass directories as archive inputs",
            path.display()
        );
        assert!(
            source.contains("bundle.hya.md extensions/runtime.js"),
            "{} must enumerate regular closure files as archive inputs",
            path.display()
        );
    }
}

#[test]
fn bundle_authoring_docs_capture_hook_and_entrypoint_contract() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("crates/hya-server/src/compat/skill_templates/agent-bundle-authoring.md"),
    ];
    let required_markers = [
        ("hook_refs", "`hook_refs`"),
        (
            "supported hook IDs",
            "supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`",
        ),
        ("hook aliases", "aliases do not rename hooks"),
        ("owning bundle", "owning bundle"),
        ("exact path", "exact-path"),
        (
            "selected entrypoints",
            "only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list",
        ),
        ("staging", "staged does not mean activated"),
        (
            "declaration validation",
            "Tool and Hook initialize declarations independently equal the selected expected sets regardless of order; missing, extra, duplicate, or unselected declarations reject",
        ),
        ("tool-only", "tool-only reports zero hooks"),
        ("hook-only", "hook-only reports zero tools"),
        (
            "multi-agent Markdown prompts",
            "empty Markdown body plus explicit per-agent `prompt:` paths enables multiple agents",
        ),
        ("bun-disjoint link/name", "bun-disjoint"),
        (
            "generic superset modules",
            "generic superset modules are rejected and must be split",
        ),
        (
            "self-contained public JS profile",
            "The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists.",
        ),
        (
            "external single-file bundling",
            "external single-file bundling",
        ),
        (
            "authoring-tree isolation",
            "activation never executes the authoring tree",
        ),
        (
            "undeclared directory files",
            "undeclared directory files are ignored",
        ),
        (
            "unreferenced archive files",
            "unreferenced archive files are rejected",
        ),
        (
            "missing relative helper import",
            "missing relative helper import fails before ACK",
        ),
        (
            "Bundle-local hook refs",
            "`hook_refs` select Bundle-local Hook resources only",
        ),
        (
            "harness hook rejection",
            "all `harness:hook/*` spellings reject",
        ),
        (
            "Harness host hooks",
            "Harness host hooks stay outside AgentBundle metadata",
        ),
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "bundle authoring documentation must exist at {}: {error}",
                path.display()
            )
        });
        for (label, marker) in required_markers {
            assert!(
                source.contains(marker),
                "{} must contain docs contract marker `{label}`: {marker}",
                path.display()
            );
        }
        assert!(
            !source.contains("validated transitive referenced closure"),
            "{} must not retain stale transitive-closure wording",
            path.display()
        );
    }
}

#[test]
fn bundle_sidecar_docs_distinguish_jsonrpc_and_plugin_protocol_versions() {
    let paths = [
        repository_root().join("docs/agent-bundle-authoring.md"),
        repository_root()
            .join("crates/hya-server/src/compat/skill_templates/agent-bundle-authoring.md"),
        repository_root().join("docs/architecture/runtime.md"),
    ];
    let required_markers = [
        "newline-delimited JSON-RPC 2.0",
        "plugin protocol version 1",
        "initialize retains existing protocol_version and host fields",
        "the only activation-specific metadata is { activation_id, lifecycle }",
    ];
    let stale_markers = [
        "JSON-RPC v1",
        "Initialization is request/reply and carries exactly activation_id and lifecycle",
        "Initialization is request/reply and carries only activation_id and lifecycle",
        "Initialize is a request/reply ACK carrying only activation_id and lifecycle",
    ];

    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "sidecar protocol documentation must exist at {}: {error}",
                path.display()
            )
        });
        let normalized = source
            .replace('`', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for marker in required_markers {
            assert!(
                normalized.contains(marker),
                "{} must contain sidecar protocol marker `{marker}`",
                path.display()
            );
        }
        for marker in stale_markers {
            assert!(
                !normalized.contains(marker),
                "{} must not retain stale sidecar protocol wording `{marker}`",
                path.display()
            );
        }
    }
}

#[test]
fn bundle_cli_docs_distinguish_catalog_publication_from_activation_closure() {
    let source = std::fs::read_to_string(repository_root().join("docs/cli.md"))
        .unwrap_or_else(|error| panic!("CLI documentation must exist: {error}"));
    let normalized = source
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    for marker in [
        "publication validates the complete built-ins-plus-installed BundleCatalog",
        "activation materializes only the selected agent's captured Tool/Hook capability closure and exact-path-matched JS Extension entrypoints",
    ] {
        assert!(
            normalized.contains(marker),
            "docs/cli.md must contain CLI contract marker `{marker}`"
        );
    }
    assert!(
        !normalized.contains("Publication and activation compile only the selected agent"),
        "docs/cli.md must not combine publication and activation into one selected closure"
    );
}

struct ExpectedAgent {
    stable_id: &'static str,
    role: AgentRole,
    lifecycle: SpawnLifecycle,
    can_spawn: &'static [&'static str],
}

struct ExpectedExample {
    directory: &'static str,
    bundle_id: &'static str,
    agents: &'static [ExpectedAgent],
}

const NO_SPAWN: &[&str] = &[];
const TRANSIENT_AGENTS: &[ExpectedAgent] = &[ExpectedAgent {
    stable_id: "docs-bun-transient",
    role: AgentRole::Main,
    lifecycle: SpawnLifecycle::Transient,
    can_spawn: NO_SPAWN,
}];
const RESIDENT_AGENTS: &[ExpectedAgent] = &[ExpectedAgent {
    stable_id: "docs-bun-resident",
    role: AgentRole::Main,
    lifecycle: SpawnLifecycle::Resident,
    can_spawn: NO_SPAWN,
}];
const BUN_EXAMPLES: &[ExpectedExample] = &[
    ExpectedExample {
        directory: "bun-transient",
        bundle_id: "hya/docs-bun-transient",
        agents: TRANSIENT_AGENTS,
    },
    ExpectedExample {
        directory: "bun-resident",
        bundle_id: "hya/docs-bun-resident",
        agents: RESIDENT_AGENTS,
    },
];

fn prepare_bun_example(directory: &str) -> (PreparedCatalog, PreparedCatalog) {
    let path = repository_root().join("docs/examples").join(directory);
    let source = BundleSource::read_directory(&path).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must exist: {error}",
            path.display()
        )
    });
    let first = prepare_package(source.clone()).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must prepare: {error}",
            path.display()
        );
    });
    let second = prepare_package(source).unwrap_or_else(|error| {
        panic!(
            "Bun example directory {} must prepare deterministically: {error}",
            path.display()
        );
    });
    (first, second)
}

#[test]
fn bun_examples_are_prepare_valid_and_deterministic() {
    for expected in BUN_EXAMPLES {
        let (first, second) = prepare_bun_example(expected.directory);
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.bundles().len(), 1);
        let bundle = &first.bundles()[0];
        assert_eq!(bundle.identity.id, expected.bundle_id);
        assert_eq!(bundle.tools.len(), 1);
        assert_eq!(bundle.extensions.len(), 1);
        assert!(bundle.skills.is_empty());
        assert!(bundle.hooks.is_empty());
        assert!(bundle.mcp.is_empty());

        {
            let agent = &bundle.agent;
            let [expected_agent] = expected.agents else {
                panic!("a bundle example declares exactly one agent");
            };
            assert_eq!(agent.id.as_str(), expected_agent.stable_id);
            assert_eq!(agent.role, expected_agent.role);
            assert_eq!(agent.spawn_lifecycle, expected_agent.lifecycle);
            let can_spawn = agent
                .can_spawn
                .iter()
                .map(|agent| agent.as_str())
                .collect::<Vec<_>>();
            assert_eq!(can_spawn.as_slice(), expected_agent.can_spawn);
        }

        let tool = &bundle.tools[0];
        assert_eq!(tool.local_id, "echo");
        assert_eq!(tool.source_path, "extensions/runtime.js");
        assert_eq!(
            tool.stable_id,
            format!("bundle:{}/tool/echo", expected.bundle_id)
        );
        assert!(!tool.content.trim().is_empty());
        assert!(!tool.digest.trim().is_empty());

        let extension = &bundle.extensions[0];
        assert_eq!(extension.local_id, "runtime");
        assert_eq!(extension.source_path, "extensions/runtime.js");
        assert_eq!(
            extension.stable_id,
            format!("bundle:{}/extension/runtime", expected.bundle_id)
        );
        assert!(extension.content.contains("export default"));
        assert!(!extension.digest.trim().is_empty());
        assert_eq!(tool.content, extension.content);
        assert_eq!(tool.digest, extension.digest);
    }
}

#[test]
fn bun_disjoint_example_is_prepare_valid_and_captures_the_agent_closure() {
    let (first, second) = prepare_bun_example("bun-disjoint");
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.bundles().len(), 1);

    let bundle = &first.bundles()[0];
    assert_eq!(bundle.identity.id, "hya/docs-bun-disjoint");
    assert_eq!(bundle.tools.len(), 1);
    assert_eq!(bundle.hooks.len(), 1);
    assert_eq!(bundle.extensions.len(), 1);
    assert!(bundle.skills.is_empty());
    assert!(bundle.mcp.is_empty());

    fn find_resource<'a>(
        resources: &'a [hya_bundle::PreparedResource],
        local_id: &str,
    ) -> &'a hya_bundle::PreparedResource {
        resources
            .iter()
            .find(|resource| resource.local_id == local_id)
            .unwrap_or_else(|| panic!("prepared resource `{local_id}` is missing"))
    }
    let assert_matches_extension = |resource: &hya_bundle::PreparedResource| {
        let extension = &bundle.extensions[0];
        assert_eq!(resource.source_path, extension.source_path);
        assert_eq!(resource.content, extension.content);
        assert_eq!(resource.digest, extension.digest);
    };

    let alpha = &bundle.agent;
    assert_eq!(alpha.id.as_str(), "docs-bun-alpha");
    assert_eq!(alpha.role, AgentRole::Main);
    assert_eq!(alpha.spawn_lifecycle, SpawnLifecycle::Transient);
    // The Markdown body is the prompt; the agent names no prompt resource.
    assert_eq!(alpha.prompt_source.as_deref(), Some("bundle.hya.md"));
    assert_eq!(
        alpha.resource_view.allow,
        ["bundle:hya/docs-bun-disjoint/tool/echo"]
    );
    assert_eq!(alpha.hook_refs, ["bundle:hya/docs-bun-disjoint/hook/event"]);

    let echo = find_resource(&bundle.tools, "echo");
    assert_eq!(echo.stable_id, "bundle:hya/docs-bun-disjoint/tool/echo");
    assert_matches_extension(echo);

    let event = find_resource(&bundle.hooks, "event");
    assert_eq!(event.stable_id, "bundle:hya/docs-bun-disjoint/hook/event");
    assert_matches_extension(event);
}
