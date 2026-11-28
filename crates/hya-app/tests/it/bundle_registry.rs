//! Bundle registry generations, conflict/policy checks, uninstall, and concurrency.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::bundle_registry::{
    BundleInstallAction, BundleInstallCandidate, BundleInstallOutcome, BundleRegistry,
    BundleRegistryError, BundleUninstallOutcome, NamespaceInstallPolicy,
};
use hya_bundle::{BundleError, BundleSource, SourceFile, prepare_package};
use sqlx::{Connection, SqliteConnection};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-bundle-registry-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn agent_candidate(
    bundle_id: &str,
    agent_id: &str,
    namespace: Option<&str>,
    version: &str,
    source_digest: [u8; 32],
    installed_at: i64,
) -> BundleInstallCandidate {
    let ns = namespace
        .map(|value| format!("namespace: {value}\n"))
        .unwrap_or_default();
    let manifest = format!(
        "kind: Bundle\nschema_version: 2\nidentity:\n  id: {bundle_id}\n  version: {version}\n  publisher: hya\n{ns}agents:\n  - id: {agent_id}\n    role: main\n    prompt: prompts/lead.md\n"
    );
    let prepared = prepare_package(BundleSource::new(
        bundle_id,
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("prompts/lead.md", b"Lead.\n".to_vec()),
        ],
    ))
    .unwrap();
    BundleInstallCandidate {
        source_digest,
        prepared_digest: prepared.digest().to_owned(),
        prepared_bytes: prepared.bytes().to_vec(),
        installed_at,
    }
}

fn workflow_candidate(source_digest: [u8; 32]) -> BundleInstallCandidate {
    let prepared = prepare_package(BundleSource::new(
        "workflow-installed",
        vec![
            SourceFile::new(
                "bundle.yaml",
                br#"kind: Bundle
schema_version: 2
identity: { id: hya/workflow-installed, version: 1.0.0, publisher: hya }
workflow: { id: release, path: workflows/release.hya.md }
agents:
  - { id: workflow-planner, role: subagent, prompt: prompts/planner.md }
  - { id: workflow-reviewer, role: subagent, prompt: prompts/reviewer.md }
"#,
            ),
            SourceFile::new(
                "workflows/release.hya.md",
                br#"---
kind: Workflow
name: release
description: Plan and review one release.
nodes:
  plan: { agent: workflow-planner, directive: Plan the release. }
  review: { agent: workflow-reviewer, directive: Review the release. }
---
flowchart TD
  plan --> review
"#,
            ),
            SourceFile::new("prompts/planner.md", b"Plan carefully.\n".as_slice()),
            SourceFile::new("prompts/reviewer.md", b"Review carefully.\n".as_slice()),
        ],
    ))
    .unwrap();
    BundleInstallCandidate {
        source_digest,
        prepared_digest: prepared.digest().to_owned(),
        prepared_bytes: prepared.bytes().to_vec(),
        installed_at: 1_725_000_010,
    }
}

#[tokio::test]
async fn install_plan_idempotency_conflict_upgrade_and_downgrade_lifecycle() {
    let registry = BundleRegistry::connect(&temp_db()).await.unwrap();
    let deny = NamespaceInstallPolicy::DenyConflicts;
    let overwrite = NamespaceInstallPolicy::OverwriteConflicts;

    let empty = registry.snapshot().await.unwrap();
    assert_eq!(empty.generation, 0);
    assert!(empty.bundles.is_empty());

    let v1 = agent_candidate("hya/pkg", "pkg-lead", Some("pkg"), "1.0.0", [0x11; 32], 100);
    let plan = registry.plan_install(&[], deny, &v1).await.unwrap();
    assert_eq!(plan.action, BundleInstallAction::Install);
    assert!(plan.displaced.is_empty());
    assert_eq!(registry.snapshot().await.unwrap().generation, 0);

    // Install + idempotent repeat.
    assert_eq!(
        registry.install(&[], deny, v1.clone()).await.unwrap(),
        BundleInstallOutcome::Installed { generation: 1 }
    );
    assert_eq!(
        registry.plan_install(&[], deny, &v1).await.unwrap().action,
        BundleInstallAction::Unchanged
    );
    assert_eq!(
        registry.install(&[], deny, v1.clone()).await.unwrap(),
        BundleInstallOutcome::Unchanged { generation: 1 }
    );

    // Same version, different digest -> content conflict without mutation.
    let v1_mutated = agent_candidate("hya/pkg", "pkg-lead", Some("pkg"), "1.0.0", [0x22; 32], 101);
    assert!(matches!(
        registry.install(&[], deny, v1_mutated).await,
        Err(BundleRegistryError::BundleContentConflict { ref bundle_id, ref version })
            if bundle_id == "hya/pkg" && version == "1.0.0"
    ));
    assert_eq!(registry.snapshot().await.unwrap().generation, 1);

    // Reserved agent rejection leaves state intact.
    let v2 = agent_candidate("hya/pkg", "pkg-lead", Some("pkg"), "2.0.0", [0x33; 32], 102);
    assert!(matches!(
        registry.plan_install(&["pkg-lead"], deny, &v2).await,
        Err(BundleRegistryError::BundleAgentIdReserved { ref agent_id, .. })
            if agent_id == "pkg-lead"
    ));

    // Plan and apply upgrade.
    assert_eq!(
        registry.plan_install(&[], deny, &v2).await.unwrap().action,
        BundleInstallAction::Replace {
            installed_version: "1.0.0".into(),
        }
    );
    assert_eq!(
        registry.install(&[], deny, v2).await.unwrap(),
        BundleInstallOutcome::Replaced { generation: 2 }
    );
    let snap = registry.snapshot().await.unwrap();
    assert_eq!(snap.generation, 2);
    assert_eq!(snap.bundles[0].version, "2.0.0");

    // Downgrade requires explicit overwrite.
    let v0 = agent_candidate("hya/pkg", "pkg-lead", Some("pkg"), "0.9.0", [0x44; 32], 103);
    assert!(matches!(
        registry.install(&[], deny, v0.clone()).await,
        Err(BundleRegistryError::BundleDowngradeRequired {
            ref bundle_id,
            ref installed_version,
            ref incoming_version,
        }) if bundle_id == "hya/pkg" && installed_version == "2.0.0" && incoming_version == "0.9.0"
    ));
    assert!(registry.plan_install(&[], overwrite, &v0).await.is_ok());
    assert_eq!(
        registry.install(&[], overwrite, v0).await.unwrap(),
        BundleInstallOutcome::Replaced { generation: 3 }
    );
}

#[tokio::test]
async fn namespace_conflicts_duplicate_agents_and_distinct_namespaces() {
    let registry = BundleRegistry::connect(&temp_db()).await.unwrap();
    let deny = NamespaceInstallPolicy::DenyConflicts;
    let overwrite = NamespaceInstallPolicy::OverwriteConflicts;

    let first = agent_candidate(
        "hya/acme-tools",
        "acme-lead",
        Some("acme"),
        "1.0.0",
        [1; 32],
        10,
    );
    registry.install(&[], deny, first).await.unwrap();

    // Duplicate stable agent ID from a different bundle fails without mutating incumbent.
    let dup_agent = agent_candidate("hya/dup", "acme-lead", Some("other"), "1.0.0", [2; 32], 11);
    assert!(matches!(
        registry.install(&[], deny, dup_agent).await,
        Err(BundleRegistryError::Bundle(BundleError::DuplicateStableAgentId { ref stable_id }))
            if stable_id == "acme-lead"
    ));

    // Cross-bundle namespace collision fails under DenyConflicts, displaces incumbent under Overwrite.
    let rival = agent_candidate(
        "hya/rival",
        "rival-lead",
        Some("acme"),
        "1.0.0",
        [3; 32],
        12,
    );
    assert!(matches!(
        registry.install(&[], deny, rival.clone()).await,
        Err(BundleRegistryError::NamespaceConflict {
            ref namespace,
            ref existing_bundle_id,
            ref incoming_bundle_id,
        }) if namespace == "acme"
            && existing_bundle_id == "hya/acme-tools"
            && incoming_bundle_id == "hya/rival"
    ));
    let plan = registry.plan_install(&[], overwrite, &rival).await.unwrap();
    assert_eq!(plan.action, BundleInstallAction::Install);
    assert_eq!(plan.displaced, vec!["hya/acme-tools".to_string()]);
    assert!(matches!(
        registry.install(&[], overwrite, rival).await.unwrap(),
        BundleInstallOutcome::Installed { .. }
    ));

    // Distinct namespace installs alongside cleanly.
    let second = agent_candidate("hya/two", "two-lead", Some("two"), "1.0.0", [4; 32], 13);
    registry.install(&[], deny, second).await.unwrap();
    let ids: Vec<_> = registry
        .snapshot()
        .await
        .unwrap()
        .bundles
        .into_iter()
        .map(|b| b.bundle_id)
        .collect();
    assert!(ids.contains(&"hya/rival".to_string()));
    assert!(ids.contains(&"hya/two".to_string()));
    assert!(!ids.contains(&"hya/acme-tools".to_string()));
}

#[tokio::test]
async fn workflow_bundle_install_and_reserved_agent_rejection() {
    let registry = BundleRegistry::connect(&temp_db()).await.unwrap();
    let deny = NamespaceInstallPolicy::DenyConflicts;

    // Reserved agent anywhere in the workflow's closure rejects the whole install.
    assert!(matches!(
        registry
            .install(&["workflow-reviewer"], deny, workflow_candidate([0x92; 32]))
            .await,
        Err(BundleRegistryError::BundleAgentIdReserved { ref agent_id, .. })
            if agent_id == "workflow-reviewer"
    ));
    assert_eq!(registry.snapshot().await.unwrap().generation, 0);

    // Valid install stores one row with workflow + both agents.
    assert_eq!(
        registry
            .install(&[], deny, workflow_candidate([0x91; 32]))
            .await
            .unwrap(),
        BundleInstallOutcome::Installed { generation: 1 }
    );
    let snapshot = registry.snapshot_with_catalogs().await.unwrap();
    assert_eq!(snapshot.generation, 1);
    let [loaded] = snapshot.bundles.as_slice() else {
        panic!("expected one workflow bundle row");
    };
    let bundle = &loaded.prepared.bundles()[0];
    assert_eq!(bundle.workflow().map(|w| w.id.as_str()), Some("release"));
    assert_eq!(bundle.agents().len(), 2);
}

#[tokio::test]
async fn uninstall_and_disabled_bundle_set_advance_generation() {
    let registry = BundleRegistry::connect(&temp_db()).await.unwrap();
    assert!(matches!(
        registry.uninstall("hya/missing").await,
        Err(BundleRegistryError::BundleNotFound { ref bundle_id }) if bundle_id == "hya/missing"
    ));

    let candidate = agent_candidate("hya/pkg", "pkg-lead", None, "1.0.0", [0x55; 32], 10);
    registry
        .install(&[], NamespaceInstallPolicy::DenyConflicts, candidate)
        .await
        .unwrap();
    assert_eq!(
        registry.uninstall("hya/pkg").await.unwrap(),
        BundleUninstallOutcome::Removed { generation: 2 }
    );
    assert!(registry.snapshot().await.unwrap().bundles.is_empty());

    // Disabling is idempotent and advances generation only on state change.
    assert!(registry.disabled_bundle_ids().await.unwrap().is_empty());
    assert_eq!(
        registry
            .set_bundle_enabled("hya/example", false)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        registry
            .disabled_bundle_ids()
            .await
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec!["hya/example".to_string()]
    );
    assert_eq!(
        registry
            .set_bundle_enabled("hya/example", false)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        registry
            .set_bundle_enabled("hya/example", true)
            .await
            .unwrap(),
        4
    );
    assert!(registry.disabled_bundle_ids().await.unwrap().is_empty());
}

#[tokio::test]
async fn corrupted_prepared_blob_is_reported_as_unreadable_snapshot_row() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await.unwrap();
    registry
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            agent_candidate("hya/pkg", "pkg-lead", None, "1.0.0", [0x88; 32], 10),
        )
        .await
        .unwrap();

    let mut conn = SqliteConnection::connect(&format!("sqlite://{path}"))
        .await
        .unwrap();
    let update = sqlx::query("UPDATE installed_bundle SET prepared_bytes = ? WHERE bundle_id = ?")
        .bind(vec![0_u8])
        .bind("hya/pkg")
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(update.rows_affected(), 1);
    assert_eq!(registry.generation().await.unwrap(), 1);

    let snapshot = registry.snapshot_with_catalogs().await.unwrap();
    assert!(snapshot.bundles.is_empty());
    let [row] = snapshot.unreadable.as_slice() else {
        panic!("expected one unreadable row: {snapshot:?}");
    };
    assert_eq!(row.bundle_id, "hya/pkg");
    assert!(row.error.contains("digest") && row.error.contains("please reinstall"));
}

#[tokio::test]
async fn writer_busy_and_concurrent_registry_opens() {
    // 1. Held write lock returns BundleRegistryBusy without mutation.
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await.unwrap();
    let mut writer = SqliteConnection::connect(&format!("sqlite://{path}"))
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    let result = registry
        .install(
            &[],
            NamespaceInstallPolicy::DenyConflicts,
            agent_candidate("hya/pkg", "pkg-lead", None, "1.0.0", [0x44; 32], 10),
        )
        .await;
    sqlx::query("ROLLBACK").execute(&mut writer).await.unwrap();
    assert!(matches!(
        result,
        Err(BundleRegistryError::BundleRegistryBusy)
    ));
    assert_eq!(registry.snapshot().await.unwrap().generation, 0);

    // 2. Concurrent first-open races complete migrations cleanly.
    for _ in 0..2 {
        let fresh = temp_db();
        let opened =
            futures::future::join_all((0..8).map(|_| BundleRegistry::connect(&fresh))).await;
        for reg in opened {
            let snap = reg.unwrap().snapshot().await.unwrap();
            assert_eq!(snap.generation, 0);
            assert!(snap.bundles.is_empty());
        }
    }
}
