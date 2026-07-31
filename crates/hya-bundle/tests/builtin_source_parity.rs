use std::path::{Path, PathBuf};

use hya_bundle::{
    AgentRole, BundleOrigin, BundleSource, HarnessAccess, SpawnLifecycle, prepare_builtins,
};
use hya_proto::{AgentName, Envelope, Event, EventSeq, ModelRef, Projection, SessionId};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| panic!("hya-bundle must live under <repository>/crates"))
}

fn read(path: &Path) -> String {
    let content = std::fs::read_to_string(path);
    let Ok(content) = content else {
        panic!("failed to read {}: {content:?}", path.display());
    };
    content
}

fn legacy_markdown_body(content: &str) -> &str {
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        panic!("legacy development agent is missing frontmatter");
    };
    let Some((_, body)) = rest.split_once("\n---") else {
        panic!("legacy development agent has unterminated frontmatter");
    };
    body.strip_prefix("\r\n")
        .or_else(|| body.strip_prefix('\n'))
        .unwrap_or(body)
        .trim()
}

fn frozen_description(id: &str) -> Option<&'static str> {
    match id {
        "build" => Some("The default agent. Executes tools based on configured permissions."),
        "plan" => Some("Plan mode. Disallows all edit tools."),
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

#[test]
fn builtin_prompt_sources_match_current_effective_prompt_bytes() {
    let root = repository_root();
    for id in ["explore", "compaction", "title", "summary"] {
        let legacy = read(
            &root
                .join("crates/hya-server/src/compat/agent_prompts")
                .join(format!("{id}.txt")),
        );
        let native = read(
            &root
                .join("bundles/builtin/hya-core-agents/prompts")
                .join(format!("{id}.md")),
        );
        assert_eq!(native.trim_end(), legacy.trim_end(), "native agent {id}");
    }

    for id in [
        "hya-docs",
        "hya-explorer",
        "hya-implementer",
        "hya-main",
        "hya-planner",
        "hya-release",
        "hya-reviewer",
        "hya-tester",
    ] {
        let legacy = read(&root.join(".hya/agents").join(format!("{id}.md")));
        let native = read(
            &root
                .join("bundles/builtin/hya-development/prompts")
                .join(format!("{id}.md")),
        );
        assert_eq!(native.trim(), legacy_markdown_body(&legacy), "agent {id}");
    }
}

#[test]
fn builtin_sources_prepare_the_frozen_native_agent_catalog() {
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
    assert_eq!(core.identity.version, "0.34.7");
    assert_eq!(development.identity.version, "0.34.7");
    assert_eq!(core.origin, BundleOrigin::Builtin);
    assert!(core.immutable);
    assert_eq!(development.origin, BundleOrigin::Builtin);
    assert!(development.immutable);

    assert_eq!(
        core.agents
            .iter()
            .map(|agent| agent.stable_id.as_str())
            .collect::<Vec<_>>(),
        [
            "build",
            "compaction",
            "explore",
            "general",
            "plan",
            "summary",
            "title",
        ]
    );
    assert_eq!(
        development
            .agents
            .iter()
            .map(|agent| agent.stable_id.as_str())
            .collect::<Vec<_>>(),
        [
            "hya-docs",
            "hya-explorer",
            "hya-implementer",
            "hya-main",
            "hya-planner",
            "hya-release",
            "hya-reviewer",
            "hya-tester",
        ]
    );

    let ordinary = [
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
    for agent in core.agents.iter().chain(&development.agents) {
        let id = agent.stable_id.as_str();
        let expected_role = match id {
            "build" | "plan" | "hya-main" => AgentRole::Main,
            _ => AgentRole::Subagent,
        };
        assert_eq!(agent.role, expected_role, "role for {id}");
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
        assert_eq!(
            agent.description.as_deref(),
            frozen_description(id),
            "description for {id}"
        );

        let can_spawn = agent
            .can_spawn
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>();
        if matches!(id, "compaction" | "title" | "summary") {
            assert!(can_spawn.is_empty(), "reserved system agent {id}");
        } else {
            assert_eq!(can_spawn, ordinary, "can_spawn for {id}");
        }
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
    for id in [
        "build",
        "compaction",
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
        "summary",
        "title",
    ] {
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
