//! Frozen legacy characterization for the native built-in catalog cutover.
//!
//! Commit 2 deletes live legacy oracles (`.hya/agents` and former
//! `hya-server` agent prompt sources). Parity is checked against immutable
//! fixtures under `tests/fixtures/legacy_characterization/`, independent of
//! those deleted paths and not generated from `bundles/builtin` at test time.

use std::path::{Path, PathBuf};

use hya_bundle::{
    AgentRole, BundleOrigin, BundleSource, HarnessAccess, PreparedAgent, SpawnLifecycle,
    prepare_builtins,
};
use hya_proto::{AgentName, Envelope, Event, EventSeq, ModelRef, Projection, SessionId};
use sha2::{Digest, Sha256};

/// Ordinary agents that may appear in each other's `can_spawn` graph.
const ORDINARY_STABLE_IDS: &[&str] = &[
    "build",
    "explore",
    "general",
    "hya-docs",
    "hya-explorer",
    "hya-implementer",
    "hya-main",
    "hya-planner",
    "hya-release",
    "hya-reviewer",
    "hya-tester",
    "plan",
];

/// Seven core product agents shipped by `hya/core-agents`.
const CORE_STABLE_IDS: &[&str] = &[
    "build",
    "compaction",
    "explore",
    "general",
    "plan",
    "summary",
    "title",
];

/// Eight development agents formerly discovered from tracked `.hya/agents`.
const DEVELOPMENT_STABLE_IDS: &[&str] = &[
    "hya-docs",
    "hya-explorer",
    "hya-implementer",
    "hya-main",
    "hya-planner",
    "hya-release",
    "hya-reviewer",
    "hya-tester",
];

/// Reserved system agents: no ordinary inbound `can_spawn` reachability.
const RESERVED_SYSTEM_IDS: &[&str] = &["compaction", "title", "summary"];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("hya-bundle must live under <repository>/crates"))
}

/// Exact prepared prompt payload frozen from pre-cutover effective sources.
///
/// Fixtures are independent of `bundles/builtin` and of deleted legacy paths.
fn frozen_prompt(id: &str) -> Option<&'static str> {
    match id {
        "build" | "plan" | "general" => None,
        "compaction" => Some(include_str!(
            "fixtures/legacy_characterization/core/compaction.md"
        )),
        "explore" => Some(include_str!(
            "fixtures/legacy_characterization/core/explore.md"
        )),
        "summary" => Some(include_str!(
            "fixtures/legacy_characterization/core/summary.md"
        )),
        "title" => Some(include_str!(
            "fixtures/legacy_characterization/core/title.md"
        )),
        "hya-docs" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-docs.md"
        )),
        "hya-explorer" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-explorer.md"
        )),
        "hya-implementer" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-implementer.md"
        )),
        "hya-main" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-main.md"
        )),
        "hya-planner" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-planner.md"
        )),
        "hya-release" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-release.md"
        )),
        "hya-reviewer" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-reviewer.md"
        )),
        "hya-tester" => Some(include_str!(
            "fixtures/legacy_characterization/development/hya-tester.md"
        )),
        _ => panic!("unexpected frozen agent {id}"),
    }
}

/// Pre-cutover characterization of the `plan` description.
///
/// Historical claim only: native PermissionPlane/resource behavior never enforced
/// a plan-specific edit prohibition. Owner-approved C_DOC_ONLY correction for 0.34.8
/// replaces the live string; do not reintroduce this claim without real enforcement.
const PRE_CUTOVER_PLAN_DESCRIPTION: &str = "Plan mode. Disallows all edit tools.";

fn frozen_description(id: &str) -> Option<&'static str> {
    match id {
        "build" => Some("The default agent. Executes tools based on configured permissions."),
        // Owner-approved documentation correction (C_DOC_ONLY): truthful planning
        // wording only. Pre-cutover text is preserved in PRE_CUTOVER_PLAN_DESCRIPTION.
        "plan" => Some("Plan mode. Planning-focused agent for designs and task breakdowns."),
        "general" => Some(
            "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.",
        ),
        "explore" => Some(
            "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.",
        ),
        "hya-docs" => Some(
            "Transient subagent for requested documentation, API docs, glossary, and ADR updates.",
        ),
        "hya-explorer" => Some(
            "Transient subagent for codebase reconnaissance, flows, conventions, and blast radius.",
        ),
        "hya-implementer" => Some(
            "Transient subagent for focused code changes after scope and target files are clear.",
        ),
        "hya-main" => Some(
            "Default primary agent for coding work. Delegates to specialist subagents and integrates verified results.",
        ),
        "hya-planner" => Some(
            "Transient subagent for design tradeoffs, implementation plans, and task breakdowns.",
        ),
        "hya-release" => {
            Some("Transient subagent for version, changelog, tag, and release readiness work.")
        }
        "hya-reviewer" => Some(
            "Transient subagent for correctness, standards, security, and simplification review.",
        ),
        "hya-tester" => {
            Some("Transient subagent for TDD tests, behavioral coverage, and focused verification.")
        }
        "compaction" | "title" | "summary" => None,
        _ => panic!("unexpected frozen agent {id}"),
    }
}

fn frozen_role(id: &str) -> AgentRole {
    match id {
        "build" | "plan" | "hya-main" => AgentRole::Main,
        _ => AgentRole::Subagent,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        assert!(write!(encoded, "{byte:02x}").is_ok());
    }
    encoded
}

fn prepare_native_builtins() -> hya_bundle::PreparedCatalog {
    let root = repository_root();
    let core = BundleSource::read_directory(root.join("bundles/builtin/hya-core-agents"));
    let Ok(core) = core else {
        panic!("failed to read core builtins: {core:?}");
    };
    let development = BundleSource::read_directory(root.join("bundles/builtin/hya-development"));
    let Ok(development) = development else {
        panic!("failed to read development builtins: {development:?}");
    };
    let prepared = prepare_builtins(vec![development, core]);
    let Ok(prepared) = prepared else {
        panic!("failed to prepare builtin catalog: {prepared:?}");
    };
    prepared
}

/// Assert prepared agent fields against frozen pre-cutover characterization.
fn assert_agent_matches_frozen_characterization(agent: &PreparedAgent) {
    let id = agent.stable_id.as_str();
    assert_eq!(agent.local_id, id, "local_id for {id}");
    assert_eq!(agent.role, frozen_role(id), "role for {id}");
    assert_eq!(
        agent.role.selector_mode(),
        match frozen_role(id) {
            AgentRole::Main => "primary",
            AgentRole::Subagent => "subagent",
        },
        "selector visibility mode for {id}"
    );
    assert_eq!(
        agent.spawn_lifecycle,
        SpawnLifecycle::Transient,
        "spawn lifecycle for {id}"
    );
    assert_eq!(
        agent.harness_access,
        HarnessAccess::Full,
        "Harness access for {id}"
    );
    assert_eq!(agent.model_policy.model, None, "model for {id}");
    assert_eq!(agent.model_policy.category, None, "category for {id}");
    assert_eq!(agent.model_policy.reasoning, None, "reasoning for {id}");
    assert_eq!(agent.workdir, None, "workdir for {id}");
    assert_eq!(agent.color, None, "color for {id}");
    assert_eq!(
        agent.description.as_deref(),
        frozen_description(id),
        "description for {id}"
    );

    let expected_prompt = frozen_prompt(id);
    assert_eq!(
        agent.prompt.as_deref(),
        expected_prompt,
        "prepared prompt payload for {id}"
    );
    match expected_prompt {
        Some(prompt) => {
            let expected_digest = sha256_hex(prompt.as_bytes());
            assert_eq!(
                agent.prompt_digest.as_deref(),
                Some(expected_digest.as_str()),
                "prompt digest for {id}"
            );
            assert!(
                agent.prompt_source.is_some(),
                "prompt_source must be present when prompt is set for {id}"
            );
        }
        None => {
            assert_eq!(agent.prompt_digest, None, "prompt digest for {id}");
            assert_eq!(agent.prompt_source, None, "prompt source for {id}");
        }
    }

    let can_spawn = agent
        .can_spawn
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>();
    if RESERVED_SYSTEM_IDS.contains(&id) {
        assert!(can_spawn.is_empty(), "reserved system agent {id}");
    } else {
        assert_eq!(can_spawn, ORDINARY_STABLE_IDS, "can_spawn for {id}");
    }
}

#[test]
fn native_plan_description_does_not_claim_unimplemented_edit_prohibition() {
    // Evidence: pre-cutover catalog text claimed an edit ban that was never enforced.
    assert_eq!(
        PRE_CUTOVER_PLAN_DESCRIPTION,
        "Plan mode. Disallows all edit tools."
    );

    let prepared = prepare_native_builtins();
    let plan = prepared
        .bundles()
        .iter()
        .flat_map(|bundle| bundle.agents.iter())
        .find(|agent| agent.stable_id.as_str() == "plan")
        .unwrap_or_else(|| panic!("prepared catalog missing plan agent"));
    let description = plan
        .description
        .as_deref()
        .unwrap_or_else(|| panic!("plan agent must expose a description"));

    assert_ne!(
        description, PRE_CUTOVER_PLAN_DESCRIPTION,
        "plan description must not retain the pre-cutover unimplemented edit-prohibition claim"
    );
    let lower = description.to_ascii_lowercase();
    assert!(
        !lower.contains("disallow") && !lower.contains("edit tool"),
        "plan description must not claim an edit-tool prohibition without PermissionPlane enforcement: {description:?}"
    );
    let Some(frozen_plan_description) = frozen_description("plan") else {
        panic!("plan frozen description");
    };
    assert_eq!(
        description, frozen_plan_description,
        "plan description must match the owner-approved truthful frozen contract"
    );
}

#[test]
fn prepared_builtin_prompts_match_frozen_legacy_characterization() {
    let prepared = prepare_native_builtins();
    let agents: Vec<&PreparedAgent> = prepared
        .bundles()
        .iter()
        .flat_map(|bundle| bundle.agents.iter())
        .collect();

    for id in CORE_STABLE_IDS
        .iter()
        .chain(DEVELOPMENT_STABLE_IDS.iter())
        .copied()
    {
        let agent = agents
            .iter()
            .find(|agent| agent.stable_id.as_str() == id)
            .unwrap_or_else(|| panic!("prepared catalog missing frozen agent {id}"));
        assert_eq!(
            agent.prompt.as_deref(),
            frozen_prompt(id),
            "prepared prompt payload for {id}"
        );
        if let Some(prompt) = frozen_prompt(id) {
            let expected_digest = sha256_hex(prompt.as_bytes());
            assert_eq!(
                agent.prompt_digest.as_deref(),
                Some(expected_digest.as_str()),
                "prepared prompt digest for {id}"
            );
        }
    }
}

#[test]
fn builtin_sources_prepare_the_frozen_native_agent_catalog() {
    let prepared = prepare_native_builtins();

    assert_eq!(
        prepared
            .bundles()
            .iter()
            .map(|bundle| bundle.identity.id.as_str())
            .collect::<Vec<_>>(),
        ["hya/core-agents", "hya/development"]
    );
    let core = &prepared.bundles()[0];
    let development = &prepared.bundles()[1];
    assert_eq!(core.identity.version, "0.34.8");
    assert_eq!(development.identity.version, "0.34.8");
    assert_eq!(core.origin, BundleOrigin::Builtin);
    assert!(core.immutable);
    assert_eq!(development.origin, BundleOrigin::Builtin);
    assert!(development.immutable);

    assert_eq!(
        core.agents
            .iter()
            .map(|agent| agent.stable_id.as_str())
            .collect::<Vec<_>>(),
        CORE_STABLE_IDS
    );
    assert_eq!(
        development
            .agents
            .iter()
            .map(|agent| agent.stable_id.as_str())
            .collect::<Vec<_>>(),
        DEVELOPMENT_STABLE_IDS
    );

    for agent in core.agents.iter().chain(&development.agents) {
        assert_agent_matches_frozen_characterization(agent);
    }

    for bundle in prepared.bundles() {
        assert!(bundle.tools.is_empty());
        assert!(bundle.skills.is_empty());
        assert!(bundle.mcp.is_empty());
        assert!(bundle.hooks.is_empty());
        assert!(bundle.extensions.is_empty());
    }
}

#[test]
fn builtin_stable_ids_round_trip_through_historical_replay_and_fork_fixtures() {
    let source_session = "ses_00000000000000000000000000000001".parse::<SessionId>();
    let Ok(source_session) = source_session else {
        panic!("fixed source session ID failed: {source_session:?}");
    };
    let fork_session = "ses_00000000000000000000000000000002".parse::<SessionId>();
    let Ok(fork_session) = fork_session else {
        panic!("fixed fork session ID failed: {fork_session:?}");
    };
    for id in CORE_STABLE_IDS
        .iter()
        .chain(DEVELOPMENT_STABLE_IDS.iter())
        .copied()
    {
        let event = Event::SessionCreated {
            session: source_session,
            parent: None,
            agent: AgentName::new(id),
            model: ModelRef::new("characterization/model"),
            workdir: "/characterization".to_string(),
        };
        let encoded = serde_json::to_vec(&event);
        let Ok(encoded) = encoded else {
            panic!("event encode failed for {id}: {encoded:?}");
        };
        let decoded = serde_json::from_slice::<Event>(&encoded);
        let Ok(decoded) = decoded else {
            panic!("event decode failed for {id}: {decoded:?}");
        };
        let projection = Projection::from_events(&[Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: decoded,
        }]);
        assert_eq!(
            projection.session.agent.as_ref().map(AgentName::as_str),
            Some(id),
            "projected AgentName bytes for {id}"
        );
        let json = serde_json::from_slice::<serde_json::Value>(&encoded);
        let Ok(json) = json else {
            panic!("event JSON decode failed for {id}: {json:?}");
        };
        assert_eq!(json["agent"], id, "wire AgentName bytes for {id}");

        let Some(fork_agent) = projection.session.agent.clone() else {
            panic!("source projection lost AgentName for {id}");
        };
        let fork = Event::SessionCreated {
            session: fork_session,
            parent: None,
            agent: fork_agent,
            model: ModelRef::new("characterization/model"),
            workdir: "/characterization".to_string(),
        };
        let encoded_fork = serde_json::to_vec(&fork);
        let Ok(encoded_fork) = encoded_fork else {
            panic!("fork event encode failed for {id}: {encoded_fork:?}");
        };
        let decoded_fork = serde_json::from_slice::<Event>(&encoded_fork);
        let Ok(decoded_fork) = decoded_fork else {
            panic!("fork event decode failed for {id}: {decoded_fork:?}");
        };
        let fork_projection = Projection::from_events(&[Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: decoded_fork,
        }]);
        assert_eq!(
            fork_projection
                .session
                .agent
                .as_ref()
                .map(AgentName::as_str),
            Some(id),
            "forked AgentName bytes for {id}"
        );
    }
}

/// Product library sources must not discover or load `.hya/agents`.
///
/// Audit is test-only: no product-side legacy presence scanner is introduced.
#[test]
fn product_library_sources_do_not_reference_dot_hya_agents() {
    let root = repository_root().join("crates");
    let mut offenders = Vec::new();
    let entries = std::fs::read_dir(&root);
    let Ok(entries) = entries else {
        panic!("failed to read crates dir: {entries:?}");
    };
    for entry in entries {
        let Ok(entry) = entry else {
            panic!("failed to read crates entry: {entry:?}");
        };
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        collect_dot_hya_agents_references(&src, &mut offenders);
    }
    assert!(
        offenders.is_empty(),
        "product library sources must not reference `.hya/agents`; found: {offenders:?}"
    );
}

fn collect_dot_hya_agents_references(dir: &Path, offenders: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir);
    let Ok(entries) = entries else {
        panic!("failed to read {}: {entries:?}", dir.display());
    };
    for entry in entries {
        let Ok(entry) = entry else {
            panic!("failed to read entry under {}: {entry:?}", dir.display());
        };
        let path = entry.path();
        if path.is_dir() {
            collect_dot_hya_agents_references(&path, offenders);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let content = std::fs::read_to_string(&path);
        let Ok(content) = content else {
            panic!("failed to read {}: {content:?}", path.display());
        };
        if content.contains(".hya/agents") {
            offenders.push(path.display().to_string());
        }
    }
}
