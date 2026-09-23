//! Integration tests for `hya-backend`: bundle cli.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LIST_HEADER: &str = "NAME VERSION AGENT STATE KIND WORKFLOW";
const BUNDLE_ID: &str = "hya/valid-public";
const BUNDLE_AGENT_ID: &str = "valid-public-lead";

/// Creates and returns a data root that no other test in this binary can share.
///
/// Every test here runs on its own thread, so `std::process::id()` is constant
/// across them and `as_nanos()` is not a reliable discriminator: seven threads
/// released together observe the identical nanosecond in ~1.6% of rounds
/// (measured, minimum delta 0ns), which is how
/// `bundle_info_lists_prepared_static_resources` once hit `AlreadyExists` on
/// `fs::create_dir`. The atomic serial makes uniqueness a guarantee instead of a
/// probability — same idiom as `hya-app`'s runtime test `tempdir()`.
///
/// Returns the freshly created directory; callers must not create it again.
fn unique_data_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    static NEXT_DATA_ROOT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let serial = NEXT_DATA_ROOT.fetch_add(1, Ordering::Relaxed);
    let data_root = std::env::temp_dir().join(format!(
        "hya-backend-bundle-cli-{}-{nanos}-{serial}",
        std::process::id()
    ));
    fs::create_dir(&data_root)?;
    Ok(data_root)
}

/// Build one minimal packaged Workflow source for installed CLI presentation tests.
fn workflow_bundle_source() -> hya_bundle::BundleSource {
    hya_bundle::BundleSource::new(
        "workflow-info",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: WorkflowBundle
identity:
  id: hya/workflow-info
  version: 1.0.0
  publisher: hya
workflow:
  id: demo
  path: workflows/demo.hya.md
agents:
  - id: demo-worker
    role: subagent
    prompt: prompts/worker.md
    spawn_lifecycle: transient
"#,
            ),
            hya_bundle::SourceFile::new(
                "workflows/demo.hya.md",
                br#"---
kind: Workflow
name: demo
description: Show one installed Workflow.
nodes:
  execute:
    agent: demo-worker
    directive: Execute.
---
flowchart TD
  execute
"#,
            ),
            hya_bundle::SourceFile::new(
                "prompts/worker.md",
                b"Execute the Workflow stage.\n".as_slice(),
            ),
        ],
    )
}

/// Build a package that tries to replace the immutable first-party bundle id.
fn first_party_collision_source() -> hya_bundle::BundleSource {
    hya_bundle::BundleSource::new(
        "first-party-collision",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/core-agents
  version: 9.9.9
  publisher: attacker
agent:
  id: collision-agent
  role: main
  prompt: prompts/collision.md
  spawn_lifecycle: transient
"#,
            ),
            hya_bundle::SourceFile::new(
                "prompts/collision.md",
                b"This package must not replace a first-party bundle.\n".as_slice(),
            ),
        ],
    )
}

fn goal_loop_override_source() -> hya_bundle::BundleSource {
    hya_bundle::BundleSource::new(
        "goal-loop-override",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: hya/goal-loop, version: 9.0.0, publisher: local }
namespace: goal-loop
resources:
  skills: [{ id: evaluator-prompt, path: evaluator.md }]
"#,
            ),
            hya_bundle::SourceFile::new(
                "evaluator.md",
                b"---\nname: evaluator-prompt\ndescription: override\n---\nCLI_OVERRIDE\n",
            ),
        ],
    )
}

fn bundle_command(data_root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya-backend"));
    command
        .env("XDG_DATA_HOME", data_root)
        .env("HOME", data_root);
    command
}

/// Fixture Claude Code plugin source directory (`fixtures/claude-plugin/`).
fn claude_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-plugin")
}

/// The `--claude` path shells out to Bun for the offline translation; when
/// Bun is unavailable (restricted environments) the CLI tests self-skip.
fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A package that claims the `demo` namespace under a different bundle id, so
/// a subsequent `--claude` install must hit the namespace conflict policy.
fn namespace_napper_package(data_root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = hya_bundle::BundleSource::new(
        "namespace-napper",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/demo-napper
  version: 1.0.0
  publisher: hya
namespace: demo
agent:
  id: napper
  role: main
  spawn_lifecycle: transient
  prompt: prompts/nap.md
"#,
            ),
            hya_bundle::SourceFile::new("prompts/nap.md", b"Take a nap.\n"),
        ],
    );
    let package = data_root.join("demo-napper.hyabundle");
    fs::write(&package, hya_bundle::write_public_package(&source)?)?;
    Ok(package)
}

fn assert_success(action: &str, output: &Output) {
    assert!(
        output.status.success(),
        "bundle {action} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn bundle_install_list_info_uninstall_workflow() -> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = write_fixture(&data_root)?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert_success("install", &install);
    let install_stdout = String::from_utf8(install.stdout)?;
    for expected in [BUNDLE_ID, "1.0.0", "generation=1"] {
        assert!(
            install_stdout.contains(expected),
            "install stdout omitted {expected:?}:\n{install_stdout}"
        );
    }

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("list", &list);
    let list_stdout = String::from_utf8(list.stdout)?;
    let mut list_lines = list_stdout.lines();
    assert_eq!(
        list_lines.next(),
        Some(LIST_HEADER),
        "unexpected bundle list header:\n{list_stdout}"
    );
    let installed_row = list_lines.find(|line| line.split_whitespace().next() == Some(BUNDLE_ID));
    assert_eq!(
        installed_row.map(|line| line.split_whitespace().collect::<Vec<_>>()),
        Some(vec![
            BUNDLE_ID,
            "1.0.0",
            BUNDLE_AGENT_ID,
            "active",
            "AgentBundle",
            "-",
        ]),
        "bundle list omitted the installed row:\n{list_stdout}"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", BUNDLE_ID])
        .output()?;
    assert_success("info", &info);
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in [
        "name=hya/valid-public",
        "version=1.0.0",
        "publisher=hya",
        "origin=installed",
        "format=public-v1",
        "state=active",
        "immutable=false",
        "source_digest=df26abcab48d8f192f7f6af59fedc3445a5254d3f4b9e765b2676143b8ce5592",
        "prepared_digest=dcfeeea231160edc585189c7568f7234b4136d095ffd97c8db99cdff2e802a92",
        "kind=AgentBundle",
        "agent=valid-public-lead",
    ] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "bundle info omitted {expected:?}:\n{info_stdout}"
        );
    }

    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", BUNDLE_ID])
        .output()?;
    assert_success("uninstall", &uninstall);
    let uninstall_stdout = String::from_utf8(uninstall.stdout)?;
    assert!(
        uninstall_stdout.contains("generation=2"),
        "uninstall stdout omitted generation=2:\n{uninstall_stdout}"
    );

    let final_list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("final list", &final_list);
    let final_list_stdout = String::from_utf8(final_list.stdout)?;
    assert!(
        !final_list_stdout.contains(BUNDLE_ID),
        "uninstalled bundle remained listed:\n{final_list_stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[tokio::test]
async fn private_info_is_opaque_and_install_does_not_mutate_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let private_package = data_root.join("private.hyabundle");
    fs::write(&private_package, private_v1_envelope())?;
    let registry_dir = data_root.join("hya/bundles");
    let registry_path = registry_dir.join("registry.sqlite3");

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "-f"])
        .arg(&private_package)
        .output()?;
    assert_success("private info", &info);
    assert!(
        !registry_path.exists(),
        "private file inspection created a bundle registry"
    );
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in [
        "format: private-v1",
        "target: x86_64-unknown-linux-gnu",
        "protocol_minimum: 1",
        "protocol_maximum: 1",
        "ciphertext_length: 1",
        "ciphertext_digest: 6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
        "authentication: unverified",
        "payload: opaque",
        concat!("activation: unsupported-in-", env!("CARGO_PKG_VERSION")),
    ] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "private bundle info omitted {expected:?}:\n{info_stdout}"
        );
    }
    assert_eq!(
        info_stdout
            .lines()
            .filter(|line| line.starts_with("ciphertext"))
            .collect::<Vec<_>>(),
        vec![
            "ciphertext_length: 1",
            "ciphertext_digest: 6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d",
        ],
        "private bundle info exposed ciphertext beyond length and digest:\n{info_stdout}"
    );

    fs::create_dir_all(&registry_dir)?;
    let registry_path_string = registry_path.to_string_lossy().into_owned();
    let registry = hya_store::BundleRegistry::connect(&registry_path_string).await?;
    let before = registry.snapshot().await?;
    assert_eq!(before.generation, 0);
    assert!(before.bundles.is_empty());

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&private_package)
        .output()?;
    assert!(
        !install.status.success(),
        "private bundle install unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr),
    );
    let install_stderr = String::from_utf8(install.stderr)?;
    assert!(
        install_stderr.contains("PRIVATE_ACTIVATION_UNSUPPORTED"),
        "private bundle install omitted typed error:\n{install_stderr}"
    );
    let after = registry.snapshot().await?;
    assert_eq!(after.generation, 0);
    assert!(after.bundles.is_empty());

    drop(registry);
    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[test]
fn bundle_list_and_info_include_first_party_without_creating_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let registry_path = data_root.join("hya/bundles/registry.sqlite3");
    assert!(!registry_path.exists());

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("first-party list", &list);
    let list_stdout = String::from_utf8(list.stdout)?;
    for expected in [
        LIST_HEADER,
        "hya/base-tools 1.0.0  active Plugin -",
        "hya/channel-tools 1.0.0  active Plugin -",
        "hya/core-commands 1.0.0  active Plugin -",
        "hya/core-skills 1.0.0  active Plugin -",
        "hya/extended-tools 1.0.0  active Plugin -",
        "hya/network-tools 1.0.0  active Plugin -",
        "hya/todo-tools 1.0.0  active Plugin -",
        "hya/goal-loop 1.0.0 goal-loop-guide,goal-loop-verifier active AgentSetBundle -",
        "hya/plan-impl-review 1.0.0 plan-impl-review-implementer,plan-impl-review-planner,plan-impl-review-reviewer active WorkflowBundle plan-impl-review",
    ] {
        assert!(
            list_stdout.lines().any(|line| line == expected),
            "bundle list omitted {expected:?}:\n{list_stdout}"
        );
    }
    assert!(
        !registry_path.exists(),
        "read-only bundle list created a bundle registry"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/plan-impl-review"])
        .output()?;
    assert_success("first-party info", &info);
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in [
        "name=hya/plan-impl-review",
        "origin=first-party",
        "format=prepared-v2",
        "immutable=true",
        "kind=WorkflowBundle",
        "workflow=plan-impl-review",
    ] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "first-party bundle info omitted {expected:?}:\n{info_stdout}"
        );
    }
    assert!(
        !registry_path.exists(),
        "read-only first-party bundle info created a bundle registry"
    );

    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", "hya/plan-impl-review"])
        .output()?;
    assert!(
        !uninstall.status.success(),
        "immutable first-party uninstall unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&uninstall.stderr).contains("immutable first-party"),
        "first-party uninstall must report immutability: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    assert!(
        !registry_path.exists(),
        "rejected first-party uninstall created a bundle registry"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// Installation rejects identities reserved by immutable trusted presets.
#[test]
fn bundle_install_rejects_first_party_identity_collision_before_registry_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = data_root.join("collision.hyabundle");
    fs::write(
        &package,
        hya_bundle::write_public_package(&first_party_collision_source())?,
    )?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert!(
        !install.status.success(),
        "colliding install unexpectedly succeeded"
    );
    let stderr = String::from_utf8(install.stderr)?;
    assert!(
        stderr.contains("immutable trusted preset `hya/core-agents`"),
        "colliding install reported the wrong error:\n{stderr}"
    );
    assert!(
        !data_root.join("hya/bundles/registry.sqlite3").exists(),
        "rejected first-party collision created a bundle registry"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[test]
fn bundle_install_first_party_override_and_uninstall_restores_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = data_root.join("goal-loop-override.hyabundle");
    fs::write(
        &package,
        hya_bundle::write_public_package(&goal_loop_override_source())?,
    )?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/goal-loop"])
        .output()?;
    let info_stdout = String::from_utf8(info.stdout)?;
    assert!(info_stdout.contains("version=9.0.0\n"), "{info_stdout}");
    assert!(info_stdout.contains("origin=installed\n"), "{info_stdout}");

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    let list_stdout = String::from_utf8(list.stdout)?;
    let goal_rows = list_stdout
        .lines()
        .filter(|line| line.starts_with("hya/goal-loop "))
        .collect::<Vec<_>>();
    assert_eq!(goal_rows.len(), 1, "{list_stdout}");
    assert!(goal_rows[0].starts_with("hya/goal-loop 9.0.0 "));

    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", "hya/goal-loop"])
        .output()?;
    assert!(
        uninstall.status.success(),
        "{}",
        String::from_utf8_lossy(&uninstall.stderr)
    );

    let fallback = bundle_command(&data_root)
        .args(["bundle", "info", "hya/goal-loop"])
        .output()?;
    let fallback_stdout = String::from_utf8(fallback.stdout)?;
    assert!(
        fallback_stdout.contains("version=1.0.0\n"),
        "{fallback_stdout}"
    );
    assert!(
        fallback_stdout.contains("origin=first-party\n"),
        "{fallback_stdout}"
    );

    fs::remove_dir_all(data_root)?;
    Ok(())
}

#[test]
fn public_info_file_prepares_without_registry_mutation() -> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = write_fixture(&data_root)?;
    let registry_path = data_root.join("hya/bundles/registry.sqlite3");
    assert!(
        !registry_path.exists(),
        "fresh data root unexpectedly contains a bundle registry"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "-f"])
        .arg(&package)
        .output()?;
    assert_success("public package info", &info);
    let stdout = String::from_utf8(info.stdout)?;
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec![
            "format: public-v1",
            "name: hya/valid-public",
            "version: 1.0.0",
            "publisher: hya",
            "origin: package",
            "state: inspected",
            "immutable: false",
            "source_digest: df26abcab48d8f192f7f6af59fedc3445a5254d3f4b9e765b2676143b8ce5592",
            "prepared_digest: dcfeeea231160edc585189c7568f7234b4136d095ffd97c8db99cdff2e802a92",
            "kind: AgentBundle",
            "agent: valid-public-lead",
        ],
        "unexpected public package info:\n{stdout}"
    );
    assert!(
        !registry_path.exists(),
        "public package info created a bundle registry"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[test]
fn install_and_info_file_require_exact_lowercase_hyabundle_suffix()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let packages = [
        data_root.join("demo.HYABUNDLE"),
        data_root.join("demo.hyabundle.extra"),
    ];
    for package in &packages {
        fs::write(
            package,
            include_bytes!("../../hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z"),
        )?;
    }
    let registry_path = data_root.join("hya/bundles/registry.sqlite3");
    assert!(!registry_path.exists());
    let commands: [(&str, &[&str]); 2] = [
        ("bundle info -f", &["bundle", "info", "-f"]),
        ("bundle install", &["bundle", "install"]),
    ];

    for package in &packages {
        for &(action, args) in &commands {
            let output = bundle_command(&data_root)
                .args(args)
                .arg(package)
                .output()?;
            assert!(
                !output.status.success(),
                "{action} accepted invalid package suffix {}\nstdout:\n{}\nstderr:\n{}",
                package.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            let stderr = String::from_utf8(output.stderr)?;
            assert!(
                stderr.contains("exact lowercase .hyabundle suffix"),
                "{action} omitted the suffix error for {}:\n{stderr}",
                package.display(),
            );
            assert!(
                !registry_path.exists(),
                "{action} created a bundle registry for {}",
                package.display(),
            );
        }
    }

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[tokio::test]
async fn bundle_info_lists_prepared_static_resources() -> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let registry_parent = data_root.join("hya/bundles");
    fs::create_dir_all(&registry_parent)?;
    let registry_path = registry_parent.join("registry.sqlite3");
    let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "resource-info",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: AgentBundle
identity:
  id: hya/resource-info
  version: 1.0.0
  publisher: hya
resources:
  skills:
    - id: handbook
      path: skills/handbook.md
agent:
  id: resource-info-lead
  role: main
  spawn_lifecycle: transient
---
You are the resource info lead.
"#,
            ),
            hya_bundle::SourceFile::new(
                "skills/handbook.md",
                b"# Handbook\nUse the handbook.\n".as_slice(),
            ),
        ],
    ))?;
    let registry_path_string = registry_path.to_string_lossy().into_owned();
    let registry = hya_store::BundleRegistry::connect(&registry_path_string).await?;
    let installed = registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            hya_store::BundleInstallCandidate {
                source_digest: [0x52; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1_725_000_011,
            },
        )
        .await?;
    assert_eq!(
        installed,
        hya_store::BundleInstallOutcome::Installed { generation: 1 }
    );
    drop(registry);

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/resource-info"])
        .output()?;
    assert_success("resource bundle info", &info);
    let stdout = String::from_utf8(info.stdout)?;
    assert!(
        stdout
            .lines()
            .any(|line| line == "agent=resource-info-lead"),
        "resource bundle info omitted the stable agent:\n{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "skill=bundle:hya/resource-info/skill/handbook"),
        "resource bundle info omitted the prepared skill:\n{stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// Installed WorkflowBundle list/info output names its payload and complete Agent closure.
#[tokio::test]
async fn workflow_bundle_list_and_info_show_kind_workflow_and_agents()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let registry_parent = data_root.join("hya/bundles");
    fs::create_dir_all(&registry_parent)?;
    let registry_path = registry_parent.join("registry.sqlite3");
    let prepared = hya_bundle::prepare_package(workflow_bundle_source())?;
    let registry_path_string = registry_path.to_string_lossy().into_owned();
    let registry = hya_store::BundleRegistry::connect(&registry_path_string).await?;
    registry
        .install(
            &[],
            hya_store::NamespaceInstallPolicy::DenyConflicts,
            hya_store::BundleInstallCandidate {
                source_digest: [0x63; 32],
                prepared_digest: prepared.digest().to_owned(),
                prepared_bytes: prepared.bytes().to_vec(),
                installed_at: 1_725_000_012,
            },
        )
        .await?;
    drop(registry);

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("WorkflowBundle list", &list);
    let list_stdout = String::from_utf8(list.stdout)?;
    assert!(
        list_stdout
            .lines()
            .any(|line| line == "hya/workflow-info 1.0.0 demo-worker active WorkflowBundle demo"),
        "unexpected WorkflowBundle list:\n{list_stdout}"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/workflow-info"])
        .output()?;
    assert_success("WorkflowBundle info", &info);
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in ["kind=WorkflowBundle", "workflow=demo", "agent=demo-worker"] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "WorkflowBundle info omitted {expected:?}:\n{info_stdout}"
        );
    }

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[tokio::test]
async fn public_bun_bundle_install_publishes_resources_atomically()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = data_root.join("archive-js.hyabundle");
    fs::write(
        &package,
        include_bytes!("../../hya-bundle/tests/fixtures/packages/valid_public_bundle_js_copy.7z"),
    )?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert_success("public Bun bundle install", &install);
    let install_stdout = String::from_utf8(install.stdout)?;
    for expected in ["hya/archive-js", "1.0.0", "generation=1"] {
        assert!(
            install_stdout.contains(expected),
            "install stdout omitted {expected:?}:\n{install_stdout}"
        );
    }

    let registry_path = data_root.join("hya/bundles/registry.sqlite3");
    let registry_path_string = registry_path.to_string_lossy().into_owned();
    let registry = hya_store::BundleRegistry::connect(&registry_path_string).await?;
    let snapshot = registry.snapshot().await?;
    assert_eq!(snapshot.generation, 1);
    assert_eq!(snapshot.bundles.len(), 1);
    let Some(record) = snapshot.bundles.first() else {
        return Err("installed bundle row is missing".into());
    };
    assert_eq!(record.bundle_id, "hya/archive-js");
    let prepared =
        hya_bundle::PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)?;
    assert_eq!(prepared.bundles().len(), 1);
    let Some(bundle) = prepared.bundles().first() else {
        return Err("prepared bundle is missing".into());
    };
    assert_eq!(bundle.identity().id, "hya/archive-js");
    let [agent] = bundle.agents() else {
        return Err("prepared AgentBundle did not contain one Agent".into());
    };
    assert_eq!(agent.id.as_str(), "archive-js-lead");
    assert_eq!(bundle.tools().len(), 1);
    assert_eq!(bundle.hooks().len(), 1);
    assert_eq!(bundle.extensions().len(), 1);
    assert_eq!(
        bundle.tools()[0].stable_id,
        "bundle:hya/archive-js/tool/echo"
    );
    assert_eq!(bundle.tools()[0].content, "export const runtime = true;\n");
    assert_eq!(
        bundle.hooks()[0].stable_id,
        "bundle:hya/archive-js/hook/event"
    );
    assert_eq!(bundle.hooks()[0].content, "export const runtime = true;\n");
    assert_eq!(
        bundle.extensions()[0].stable_id,
        "bundle:hya/archive-js/extension/runtime"
    );
    assert_eq!(
        bundle.extensions()[0].content,
        "export const runtime = true;\n"
    );
    drop(registry);

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/archive-js"])
        .output()?;
    assert_success("public Bun bundle info", &info);
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in [
        "agent=archive-js-lead",
        "tool=bundle:hya/archive-js/tool/echo",
        "hook=bundle:hya/archive-js/hook/event",
        "extension=bundle:hya/archive-js/extension/runtime",
    ] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "bundle info omitted {expected:?}:\n{info_stdout}"
        );
    }

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

fn write_fixture(data_root: &Path) -> Result<PathBuf, std::io::Error> {
    let package = data_root.join("demo.hyabundle");
    fs::write(
        &package,
        include_bytes!("../../hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z"),
    )?;
    Ok(package)
}

fn private_v1_envelope() -> Vec<u8> {
    let mut bytes = b"HYABNDL\0".to_vec();
    for value in [1_u16, 1, 1, 24, 12, 16] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&[
        0x6e, 0x34, 0x0b, 0x9c, 0xff, 0xb3, 0x7a, 0x98, 0x9c, 0xa5, 0x44, 0xe6, 0xbb, 0x78, 0x0a,
        0x2c, 0x78, 0x90, 0x1d, 0x3f, 0xb3, 0x37, 0x38, 0x76, 0x85, 0x11, 0xa3, 0x06, 0x17, 0xaf,
        0xa0, 0x1d,
    ]);
    bytes.extend_from_slice(b"x86_64-unknown-linux-gnu");
    bytes.extend_from_slice(&[0; 12]);
    bytes.push(0);
    bytes.extend_from_slice(&[0; 16]);
    assert_eq!(bytes.len(), 113);
    bytes
}

/// Build one AgentBundle package declaring one JS tool and a `db` schema.
fn schema_bundle_package(
    data_root: &std::path::Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = hya_bundle::BundleSource::new(
        "schema-cli",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/schema-cli
  version: 1.0.0
  publisher: hya
schemas:
  - scheme: db
    tool: query
    writable: false
resources:
  tools:
    - id: query
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: lead
  role: main
  spawn_lifecycle: transient
  resource_view:
    allow:
      - query
      - runtime
"#,
            ),
            hya_bundle::SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
        ],
    );
    let package = data_root.join("schema-cli.hyabundle");
    fs::write(&package, hya_bundle::write_public_package(&source)?)?;
    Ok(package)
}

#[test]
fn bundle_schemas_lists_declared_scheme_extensions() -> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = schema_bundle_package(&data_root)?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert_success("install", &install);

    let schemas = bundle_command(&data_root)
        .args(["bundle", "schemas"])
        .output()?;
    assert_success("schemas", &schemas);
    let schemas_stdout = String::from_utf8(schemas.stdout)?;
    let mut lines = schemas_stdout.lines();
    assert_eq!(
        lines.next(),
        Some("BUNDLE SCHEME TOOL WRITABLE"),
        "unexpected schemas header:\n{schemas_stdout}"
    );
    let row = lines
        .find(|line| line.split_whitespace().next() == Some("hya/schema-cli"))
        .ok_or("installed bundle must be listed in bundle schemas output")?;
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["hya/schema-cli", "db", "query", "false"],
        "the schema row must carry scheme, owner tool, and writable flag:\n{schemas_stdout}"
    );

    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", "hya/schema-cli"])
        .output()?;
    assert_success("uninstall", &uninstall);
    let after = bundle_command(&data_root)
        .args(["bundle", "schemas"])
        .output()?;
    assert_success("schemas after uninstall", &after);
    let after_stdout = String::from_utf8(after.stdout)?;
    assert!(
        !after_stdout.contains("hya/schema-cli"),
        "uninstalled bundle remained listed:\n{after_stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// Build one packaged AgentBundle declaring schemas, `extensions.process`, and
/// `resources.mcp` for the `bundle info` declaration surface.
fn declaration_bundle_package(
    data_root: &std::path::Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let source = hya_bundle::BundleSource::new(
        "decl-demo",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: AgentBundle
identity:
  id: hya/decl-demo
  version: 1.0.0
  publisher: hya
schemas:
  - scheme: db
    tool: query
    writable: false
resources:
  tools:
    - id: query
      path: extensions/runtime.js
  mcp:
    - id: vecdb
      path: mcp/vecdb.json
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
  process:
    kind: bun
    command: [bun, run, extensions/runtime.ts]
agent:
  id: decl-lead
  role: main
  spawn_lifecycle: transient
  resource_view:
    allow:
      - query
      - runtime
"#,
            ),
            hya_bundle::SourceFile::new("extensions/runtime.js", b"export default {}".to_vec()),
            hya_bundle::SourceFile::new(
                "mcp/vecdb.json",
                br#"{"command": ["python3", "vecdb.py"]}"#.to_vec(),
            ),
        ],
    );
    let package = data_root.join("decl-demo.hyabundle");
    fs::write(&package, hya_bundle::write_public_package(&source)?)?;
    Ok(package)
}

/// `bundle info` reports declared schemas, the process extension, and mcp
/// entries — and prints none of those lines when the bundle declares none.
#[test]
fn bundle_info_reports_schema_process_and_mcp_declarations()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let package = declaration_bundle_package(&data_root)?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert_success("install", &install);

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/decl-demo"])
        .output()?;
    assert_success("info", &info);
    let stdout = String::from_utf8(info.stdout)?;
    let lines = stdout.lines().collect::<Vec<_>>();
    for expected in [
        "schema=db tool=query writable=false",
        "process=bun command=bun run extensions/runtime.ts",
        "mcp=bundle:hya/decl-demo/mcp/vecdb",
    ] {
        assert!(
            lines.contains(&expected),
            "bundle info omitted {expected:?}:\n{stdout}"
        );
    }

    // A bundle with no declarations prints none of the new lines.
    let plain = bundle_command(&data_root)
        .args(["bundle", "info", "hya/valid-public"])
        .output()?;
    let plain_ok = plain.status.success();
    if plain_ok {
        let plain_stdout = String::from_utf8(plain.stdout)?;
        for fragment in ["schema=", "process=", "mcp="] {
            assert!(
                !plain_stdout.lines().any(|line| line.starts_with(fragment)),
                "plain bundle info must not print {fragment:?} lines:\n{plain_stdout}"
            );
        }
    }

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// `bundle install --claude <dir>` translates the fixture plugin offline and
/// installs it as `claude/demo`; `bundle list` and `bundle info` show the
/// translated AgentSetBundle with its skills and MCP declarations.
#[test]
fn bundle_install_claude_translates_and_installs_fixture() -> Result<(), Box<dyn std::error::Error>>
{
    if !bun_available() {
        eprintln!("skipping: bun is not available");
        return Ok(());
    }
    let data_root = unique_data_root()?;

    let install = bundle_command(&data_root)
        .args(["bundle", "install", "--claude"])
        .arg(claude_fixture_dir())
        .output()?;
    assert_success("claude install", &install);
    let install_stdout = String::from_utf8(install.stdout)?;
    for expected in ["installed claude/demo 1.0.0", "generation=1"] {
        assert!(
            install_stdout.contains(expected),
            "claude install stdout omitted {expected:?}:\n{install_stdout}"
        );
    }

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("list", &list);
    let list_stdout = String::from_utf8(list.stdout)?;
    let installed_row = list_lines_starting_with(&list_stdout, "claude/demo");
    assert_eq!(
        installed_row, "claude/demo 1.0.0 reviewer active AgentSetBundle -",
        "bundle list omitted the claude/demo row:\n{list_stdout}"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "claude/demo"])
        .output()?;
    assert_success("info", &info);
    let info_stdout = String::from_utf8(info.stdout)?;
    for expected in [
        "name=claude/demo",
        "version=1.0.0",
        "publisher=claude",
        "kind=AgentSetBundle",
        "agent=reviewer",
        "skill=bundle:claude/demo/skill/audit",
        "skill=bundle:claude/demo/skill/review",
        "mcp=bundle:claude/demo/mcp/vecdb",
    ] {
        assert!(
            info_stdout.lines().any(|line| line == expected),
            "bundle info omitted {expected:?}:\n{info_stdout}"
        );
    }

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[test]
fn bundle_install_claude_resolves_local_marketplace_reference()
-> Result<(), Box<dyn std::error::Error>> {
    if !bun_available() {
        eprintln!("skipping: bun is not available");
        return Ok(());
    }
    let data_root = unique_data_root()?;
    let marketplace = data_root.join("marketplace");
    let plugin = marketplace.join("plugins/resource-only");
    fs::create_dir_all(plugin.join("skills/scan"))?;
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"market-resource","version":"1.0.0"}"#,
    )?;
    fs::write(plugin.join("skills/scan/SKILL.md"), "# Scan\n")?;
    fs::write(
        marketplace.join("marketplace.json"),
        r#"{"name":"fixture","plugins":[{"name":"resource-only","source":"./plugins/resource-only"}]}"#,
    )?;
    let reference = format!("{}#resource-only", marketplace.display());
    let install = bundle_command(&data_root)
        .args(["bundle", "install", "--claude", &reference])
        .output()?;
    assert_success("claude marketplace install", &install);
    let info = bundle_command(&data_root)
        .args(["bundle", "info", "claude/market-resource"])
        .output()?;
    assert_success("claude marketplace info", &info);
    assert!(String::from_utf8(info.stdout)?.contains("kind=Plugin"));
    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// Reinstalling the same claude source is a no-op; a foreign bundle owning
/// the `demo` namespace triggers NAMESPACE_CONFLICT guidance, and
/// `--overwrite` replaces the incumbent.
#[test]
fn bundle_install_claude_conflicts_follow_namespace_policy()
-> Result<(), Box<dyn std::error::Error>> {
    if !bun_available() {
        eprintln!("skipping: bun is not available");
        return Ok(());
    }
    let data_root = unique_data_root()?;

    // Same source twice: the registry dedupes to `unchanged`.
    let first = bundle_command(&data_root)
        .args(["bundle", "install", "--claude"])
        .arg(claude_fixture_dir())
        .output()?;
    assert_success("first claude install", &first);
    let second = bundle_command(&data_root)
        .args(["bundle", "install", "--claude"])
        .arg(claude_fixture_dir())
        .output()?;
    assert_success("second claude install", &second);
    assert!(
        String::from_utf8(second.stdout)?.contains("unchanged claude/demo"),
        "identical claude reinstall must be unchanged"
    );

    // Uninstall, then let a foreign bundle claim the `demo` namespace.
    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", "claude/demo"])
        .output()?;
    assert_success("uninstall", &uninstall);
    let napper = namespace_napper_package(&data_root)?;
    let napper_install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&napper)
        .output()?;
    assert_success("napper install", &napper_install);

    let denied = bundle_command(&data_root)
        .args(["bundle", "install", "--claude"])
        .arg(claude_fixture_dir())
        .output()?;
    assert!(
        !denied.status.success(),
        "namespace conflict install unexpectedly succeeded\nstdout:\n{}",
        String::from_utf8_lossy(&denied.stdout)
    );
    let denied_stderr = String::from_utf8(denied.stderr)?;
    assert!(
        denied_stderr.contains("NAMESPACE_CONFLICT") && denied_stderr.contains("--overwrite"),
        "conflict install omitted NAMESPACE_CONFLICT guidance:\n{denied_stderr}"
    );

    let replaced = bundle_command(&data_root)
        .args(["bundle", "install", "--overwrite", "--claude"])
        .arg(claude_fixture_dir())
        .output()?;
    assert_success("overwrite claude install", &replaced);
    let replaced_stdout = String::from_utf8(replaced.stdout)?;
    assert!(
        replaced_stdout.contains("installed claude/demo")
            || replaced_stdout.contains("replaced claude/demo"),
        "overwrite install stdout unexpected:\n{replaced_stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

fn list_lines_starting_with(stdout: &str, prefix: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `bundle search <query>` filters the merged first-party + installed catalog
/// by a case-insensitive substring over bundle ids, agent ids, and skill ids,
/// printing `bundle list`-shaped rows for matching bundles only.
#[test]
fn bundle_search_filters_first_party_and_installed_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;

    // Mixed-case bundle id match: only the goal-loop row prints.
    let by_bundle_id = bundle_command(&data_root)
        .args(["bundle", "search", "GOAL-LOOP"])
        .output()?;
    assert_success("search by bundle id", &by_bundle_id);
    let by_bundle_id_stdout = String::from_utf8(by_bundle_id.stdout)?;
    assert_eq!(
        by_bundle_id_stdout.lines().collect::<Vec<_>>(),
        vec![
            LIST_HEADER,
            "hya/goal-loop 1.0.0 goal-loop-guide,goal-loop-verifier active AgentSetBundle -",
        ],
        "unexpected bundle id search rows:\n{by_bundle_id_stdout}"
    );

    // Skill id match: `goal-contract` exists only on hya/goal-loop.
    let by_skill = bundle_command(&data_root)
        .args(["bundle", "search", "goal-contract"])
        .output()?;
    assert_success("search by skill id", &by_skill);
    let by_skill_stdout = String::from_utf8(by_skill.stdout)?;
    assert!(
        by_skill_stdout
            .lines()
            .any(|line| line.starts_with("hya/goal-loop")),
        "skill id search omitted hya/goal-loop:\n{by_skill_stdout}"
    );
    assert!(
        !by_skill_stdout.contains("hya/plan-impl-review"),
        "skill id search printed an unrelated bundle:\n{by_skill_stdout}"
    );

    // Agent id match reaches the other first-party bundle.
    let by_agent = bundle_command(&data_root)
        .args(["bundle", "search", "plan-impl-review-planner"])
        .output()?;
    assert_success("search by agent id", &by_agent);
    assert!(
        String::from_utf8(by_agent.stdout)?
            .lines()
            .any(|line| line.starts_with("hya/plan-impl-review")),
        "agent id search omitted hya/plan-impl-review"
    );

    // Installed bundles join the search surface after `bundle install`.
    let package = write_fixture(&data_root)?;
    let install = bundle_command(&data_root)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert_success("install", &install);
    let installed = bundle_command(&data_root)
        .args(["bundle", "search", "VALID-PUBLIC"])
        .output()?;
    assert_success("search installed bundle", &installed);
    let installed_stdout = String::from_utf8(installed.stdout)?;
    assert_eq!(
        installed_stdout.lines().collect::<Vec<_>>(),
        vec![
            LIST_HEADER,
            "hya/valid-public 1.0.0 valid-public-lead active AgentBundle -",
        ],
        "installed search must match the installed bundle only:\n{installed_stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// `bundle search` requires a query (missing or whitespace-only exits
/// non-zero) and `--help` prints the usage line.
#[test]
fn bundle_search_requires_query_and_answers_help() -> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;

    let missing = bundle_command(&data_root)
        .args(["bundle", "search"])
        .output()?;
    assert!(
        !missing.status.success(),
        "bundle search without a query must exit non-zero"
    );
    let missing_stderr = String::from_utf8(missing.stderr)?;
    assert!(
        missing_stderr.contains("Usage:"),
        "missing-query error must print the usage line:\n{missing_stderr}"
    );

    let blank = bundle_command(&data_root)
        .args(["bundle", "search", "   "])
        .output()?;
    assert!(
        !blank.status.success(),
        "whitespace-only query must exit non-zero"
    );

    let help = bundle_command(&data_root)
        .args(["bundle", "search", "--help"])
        .output()?;
    assert_success("search help", &help);
    let help_stdout = String::from_utf8(help.stdout)?;
    assert!(
        help_stdout.contains("Usage:") && help_stdout.contains("<QUERY>"),
        "search help must show the usage line with <QUERY>:\n{help_stdout}"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

/// When no bundle metadata matches, `bundle search` exits 0 and lists the
/// full catalog instead (with a stderr hint). `schemas` matches no
/// first-party bundle id, agent, or skill, so this pins the documented
/// fallback that keeps `bundle search schemas` showing `hya/goal-loop`.
#[test]
fn bundle_search_without_a_metadata_match_lists_the_catalog()
-> Result<(), Box<dyn std::error::Error>> {
    let data_root = unique_data_root()?;
    let registry_path = data_root.join("hya/bundles/registry.sqlite3");

    let search = bundle_command(&data_root)
        .args(["bundle", "search", "schemas"])
        .output()?;
    assert_success("no-match search", &search);
    let stdout = String::from_utf8(search.stdout)?;
    for expected in [
        LIST_HEADER,
        "hya/base-tools 1.0.0  active Plugin -",
        "hya/channel-tools 1.0.0  active Plugin -",
        "hya/core-commands 1.0.0  active Plugin -",
        "hya/core-skills 1.0.0  active Plugin -",
        "hya/extended-tools 1.0.0  active Plugin -",
        "hya/network-tools 1.0.0  active Plugin -",
        "hya/todo-tools 1.0.0  active Plugin -",
        "hya/goal-loop 1.0.0 goal-loop-guide,goal-loop-verifier active AgentSetBundle -",
        "hya/plan-impl-review 1.0.0 plan-impl-review-implementer,plan-impl-review-planner,plan-impl-review-reviewer active WorkflowBundle plan-impl-review",
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "no-match search omitted {expected:?}:\n{stdout}"
        );
    }
    assert!(
        String::from_utf8(search.stderr)?.contains("no bundle metadata matched"),
        "no-match search must explain the fallback on stderr"
    );
    assert!(
        !registry_path.exists(),
        "read-only bundle search created a bundle registry"
    );

    fs::remove_dir_all(&data_root)?;
    Ok(())
}

#[test]
fn agent_set_installs_lists_searches_and_uninstalls_as_one_package()
-> Result<(), Box<dyn std::error::Error>> {
    let root = unique_data_root()?;
    let source = hya_bundle::BundleSource::new(
        "agent-set-cli",
        vec![hya_bundle::SourceFile::new(
            "bundle.yaml",
            br#"kind: AgentSetBundle
identity: { id: acme/set-cli, version: 1.0.0, publisher: acme }
agents:
  - { id: set-cli-lead, role: main }
  - { id: set-cli-reviewer, role: subagent }
"#,
        )],
    );
    let package = root.join("team.hyabundle");
    fs::write(&package, hya_bundle::write_public_package(&source)?)?;
    let path = package.to_str().ok_or("non-UTF8 package path")?;
    for args in [
        vec!["bundle", "info", "-f", path],
        vec!["bundle", "install", path],
        vec!["bundle", "info", "acme/set-cli"],
        vec!["bundle", "list"],
        vec!["bundle", "search", "set-cli-reviewer"],
    ] {
        let output = bundle_command(&root).args(&args).output()?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("acme/set-cli"), "{args:?}: {stdout}");
        if args[1] != "install" {
            assert!(stdout.contains("AgentSetBundle"), "{args:?}: {stdout}");
            assert!(
                stdout.contains("set-cli-lead") && stdout.contains("set-cli-reviewer"),
                "{args:?}: {stdout}"
            );
        }
    }
    assert!(
        bundle_command(&root)
            .args(["bundle", "uninstall", "acme/set-cli"])
            .output()?
            .status
            .success()
    );
    let list = bundle_command(&root).args(["bundle", "list"]).output()?;
    assert!(!String::from_utf8_lossy(&list.stdout).contains("acme/set-cli"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn plugin_installs_lists_searches_and_uninstalls_without_an_agent()
-> Result<(), Box<dyn std::error::Error>> {
    let root = unique_data_root()?;
    let source = hya_bundle::BundleSource::new(
        "plugin-cli",
        vec![
            hya_bundle::SourceFile::new(
                "bundle.yaml",
                br#"kind: Plugin
identity: { id: acme/plugin-cli, version: 1.0.0, publisher: acme }
resources:
  skills:
    - id: plugin-help
      path: resources/skills/plugin-help.md
"#,
            ),
            hya_bundle::SourceFile::new(
                "resources/skills/plugin-help.md",
                b"---\nname: plugin-help\ndescription: Plugin CLI fixture.\n---\n# Plugin Help\nPlugin-only searchable skill.\n",
            ),
        ],
    );
    let package = root.join("plugin.hyabundle");
    fs::write(&package, hya_bundle::write_public_package(&source)?)?;
    let path = package.to_str().ok_or("non-UTF8 package path")?;

    for args in [
        vec!["bundle", "info", "-f", path],
        vec!["bundle", "install", path],
        vec!["bundle", "info", "acme/plugin-cli"],
        vec!["bundle", "list"],
        vec!["bundle", "search", "plugin-help"],
    ] {
        let output = bundle_command(&root).args(&args).output()?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("acme/plugin-cli"), "{args:?}: {stdout}");
        if args[1] != "install" {
            assert!(stdout.contains("Plugin"), "{args:?}: {stdout}");
            if args[1] == "info" {
                assert!(stdout.contains("plugin-help"), "{args:?}: {stdout}");
            }
        }
    }
    assert!(
        bundle_command(&root)
            .args(["bundle", "uninstall", "acme/plugin-cli"])
            .output()?
            .status
            .success()
    );
    let list = bundle_command(&root).args(["bundle", "list"]).output()?;
    assert!(!String::from_utf8_lossy(&list.stdout).contains("acme/plugin-cli"));
    fs::remove_dir_all(root)?;
    Ok(())
}
