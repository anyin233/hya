//! Bundle registry generations and installed-candidate resolution.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{
    BundleError, BundleSource, PackageInspection, PrivatePackageAuthentication,
    PrivatePackageInspection, PrivatePackagePayload, SourceFile, prepare_package,
};
use hya_store::{
    BundleInstallCandidate, BundleInstallOutcome, BundleRegistry, BundleUninstallOutcome,
    StoreError,
};
use sqlx::{Connection, SqliteConnection};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH);
    let Ok(elapsed) = elapsed else {
        panic!("system clock predates the Unix epoch");
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-bundle-registry-{}-{}-{id}.db",
            elapsed.as_nanos(),
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn installed_candidate(
    version: &str,
    description: &str,
    prompt: &str,
    source_digest: [u8; 32],
    installed_at: i64,
) -> BundleInstallCandidate {
    let source = format!(
        "---\nkind: AgentBundle\nidentity:\n  id: hya/installed-package\n  version: {version}\n  publisher: hya\nagent:\n  id: installed-package-lead\n  description: {description}\n  role: main\n  spawn_lifecycle: transient\n---\n{prompt}\n"
    );
    let prepared = prepare_package(BundleSource::new(
        "installed-package",
        vec![SourceFile::new("bundle.hya.md", source.into_bytes())],
    ));
    let Ok(prepared) = prepared else {
        panic!("installed package preparation failed: {prepared:?}");
    };
    let [bundle] = prepared.bundles() else {
        panic!("expected exactly one prepared bundle");
    };
    assert_eq!(bundle.identity.id, "hya/installed-package");
    assert_eq!(bundle.identity.version, version);
    BundleInstallCandidate {
        source_digest,
        prepared_digest: prepared.digest().to_owned(),
        prepared_bytes: prepared.bytes().to_vec(),
        installed_at,
    }
}

#[tokio::test]
async fn bundle_registry_initializes_empty_at_generation_zero() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };
    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };

    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn private_package_install_is_unsupported_without_registry_mutation() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };
    let inspection = PackageInspection::Private(PrivatePackageInspection {
        target: "x86_64-unknown-linux-gnu".to_owned(),
        protocol_minimum: 1,
        protocol_maximum: 1,
        authentication: PrivatePackageAuthentication::Unverified,
        payload: PrivatePackagePayload::Opaque,
        ciphertext_length: 1,
        ciphertext_digest: [0; 32],
    });

    let install = registry
        .install_inspection(&[], inspection, 1_725_000_009)
        .await;
    assert!(matches!(
        install,
        Err(StoreError::PrivateActivationUnsupported)
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn corrupted_prepared_blob_is_rejected_by_snapshot() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };
    let install = registry
        .install(
            &[],
            installed_candidate(
                "1.0.0",
                "installed package",
                "You are the installed package lead.",
                [0x88; 32],
                1_725_000_008,
            ),
        )
        .await;
    let Ok(install) = install else {
        panic!("bundle install failed: {install:?}");
    };
    assert_eq!(install, BundleInstallOutcome::Installed { generation: 1 });

    let connection = SqliteConnection::connect(&format!("sqlite://{path}")).await;
    let Ok(mut connection) = connection else {
        panic!("direct SQLite connection failed: {connection:?}");
    };
    let update = sqlx::query("UPDATE installed_bundle SET prepared_bytes = ? WHERE bundle_id = ?")
        .bind(vec![0_u8])
        .bind("hya/installed-package")
        .execute(&mut connection)
        .await;
    let Ok(update) = update else {
        panic!("prepared blob corruption update failed: {update:?}");
    };
    assert_eq!(update.rows_affected(), 1);

    let generation = registry.generation().await;
    let Ok(generation) = generation else {
        panic!("bundle registry generation failed: {generation:?}");
    };
    assert_eq!(generation, 1);

    let snapshot = registry.snapshot().await;
    assert!(matches!(
        snapshot,
        Err(StoreError::BundleRegistryCorrupt { bundle_id })
            if bundle_id == "hya/installed-package"
    ));
}

#[tokio::test]
async fn same_source_digest_install_is_idempotent_without_generation_advance() {
    let prepared = prepare_package(BundleSource::new(
        "installed-package",
        vec![SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/installed-package
  version: 1.0.0
  publisher: hya
agent:
  id: installed-package-lead
  role: main
  spawn_lifecycle: transient
---
You are the installed package lead.
"#,
        )],
    ));
    let Ok(prepared) = prepared else {
        panic!("installed package preparation failed: {prepared:?}");
    };
    let [bundle] = prepared.bundles() else {
        panic!("expected exactly one prepared bundle");
    };
    let candidate = BundleInstallCandidate {
        source_digest: [0x11; 32],
        prepared_digest: prepared.digest().to_owned(),
        prepared_bytes: prepared.bytes().to_vec(),
        installed_at: 1_725_000_000,
    };

    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let first = registry.install(&[], candidate.clone()).await;
    let Ok(first) = first else {
        panic!("first bundle install failed: {first:?}");
    };
    assert_eq!(first, BundleInstallOutcome::Installed { generation: 1 });

    let second = registry.install(&[], candidate.clone()).await;
    let Ok(second) = second else {
        panic!("second bundle install failed: {second:?}");
    };
    assert_eq!(second, BundleInstallOutcome::Unchanged { generation: 1 });

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 1);
    let [record] = snapshot.bundles.as_slice() else {
        panic!("expected exactly one installed bundle");
    };
    assert_eq!(record.bundle_id, bundle.identity.id);
    assert_eq!(record.version, bundle.identity.version);
    assert_eq!(record.publisher, bundle.identity.publisher);
    assert_eq!(record.source_digest, [0x11; 32]);
    assert_eq!(record.prepared_digest, prepared.digest());
    assert_eq!(record.prepared_bytes.as_slice(), prepared.bytes());
    assert_eq!(record.installed_at, 1_725_000_000);
}

#[tokio::test]
async fn same_version_different_source_digest_is_content_conflict_without_mutation() {
    let first_candidate = installed_candidate(
        "1.0.0",
        "first installed package",
        "You are the first installed package lead.",
        [0x11; 32],
        1_725_000_000,
    );
    let first_prepared_digest = first_candidate.prepared_digest.clone();
    let first_prepared_bytes = first_candidate.prepared_bytes.clone();
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let first = registry.install(&[], first_candidate).await;
    let Ok(first) = first else {
        panic!("first bundle install failed: {first:?}");
    };
    assert_eq!(first, BundleInstallOutcome::Installed { generation: 1 });

    let second_candidate = installed_candidate(
        "1.0.0",
        "second installed package",
        "You are the second installed package lead.",
        [0x22; 32],
        1_725_000_001,
    );
    let second = registry.install(&[], second_candidate).await;
    assert!(matches!(
        second,
        Err(StoreError::BundleContentConflict {
            bundle_id,
            version,
        }) if bundle_id == "hya/installed-package" && version == "1.0.0"
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 1);
    let [record] = snapshot.bundles.as_slice() else {
        panic!("expected exactly one installed bundle");
    };
    assert_eq!(record.bundle_id, "hya/installed-package");
    assert_eq!(record.version, "1.0.0");
    assert_eq!(record.source_digest, [0x11; 32]);
    assert_eq!(record.prepared_digest, first_prepared_digest);
    assert_eq!(record.prepared_bytes, first_prepared_bytes);
    assert_eq!(record.installed_at, 1_725_000_000);
}

#[tokio::test]
async fn different_version_replaces_atomically_and_advances_generation_once() {
    let version_one = installed_candidate(
        "1.0.0",
        "version one installed package",
        "You are the version one installed package lead.",
        [0x11; 32],
        1_725_000_000,
    );
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let first = registry.install(&[], version_one).await;
    let Ok(first) = first else {
        panic!("first bundle install failed: {first:?}");
    };
    assert_eq!(first, BundleInstallOutcome::Installed { generation: 1 });

    let version_two = installed_candidate(
        "2.0.0",
        "version two installed package",
        "You are the version two installed package lead.",
        [0x22; 32],
        1_725_000_002,
    );
    let replacement_digest = version_two.prepared_digest.clone();
    let replacement_bytes = version_two.prepared_bytes.clone();
    let second = registry.install(&[], version_two).await;
    let Ok(second) = second else {
        panic!("replacement bundle install failed: {second:?}");
    };
    assert_eq!(second, BundleInstallOutcome::Replaced { generation: 2 });

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 2);
    let [record] = snapshot.bundles.as_slice() else {
        panic!("expected exactly one installed bundle");
    };
    assert_eq!(record.bundle_id, "hya/installed-package");
    assert_eq!(record.version, "2.0.0");
    assert_eq!(record.source_digest, [0x22; 32]);
    assert_eq!(record.prepared_digest, replacement_digest);
    assert_eq!(record.prepared_bytes, replacement_bytes);
    assert_eq!(record.installed_at, 1_725_000_002);
}

#[tokio::test]
async fn conflicting_agent_id_install_preserves_old_row_and_generation() {
    // Two installed bundles may not export the same agent id. The clash is
    // found while revalidating the whole installed catalog, and the row that
    // was already there must survive untouched.
    let old_candidate = installed_candidate(
        "1.0.0",
        "version one installed package",
        "You are the version one installed package lead.",
        [0x66; 32],
        1_725_000_006,
    );
    let old_digest = old_candidate.prepared_digest.clone();
    let old_bytes = old_candidate.prepared_bytes.clone();
    let old_installed_at = old_candidate.installed_at;
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let first = registry.install(&[], old_candidate).await;
    let Ok(first) = first else {
        panic!("first bundle install failed: {first:?}");
    };
    assert_eq!(first, BundleInstallOutcome::Installed { generation: 1 });

    let conflicting = prepare_package(BundleSource::new(
        "replacement-conflict",
        vec![SourceFile::new(
            "bundle.hya.md",
            br#"---
kind: AgentBundle
identity:
  id: hya/replacement-conflict
  version: 1.0.0
  publisher: hya
agent:
  id: installed-package-lead
  description: conflicting lead
  role: main
  spawn_lifecycle: transient
---
You are the conflicting lead.
"#,
        )],
    ));
    let Ok(conflicting) = conflicting else {
        panic!("conflicting package preparation failed: {conflicting:?}");
    };

    let replacement = registry
        .install(
            &[],
            BundleInstallCandidate {
                source_digest: [0x77; 32],
                prepared_digest: conflicting.digest().to_owned(),
                prepared_bytes: conflicting.bytes().to_vec(),
                installed_at: 1_725_000_007,
            },
        )
        .await;
    assert!(matches!(
        replacement,
        Err(StoreError::Bundle(BundleError::DuplicateStableAgentId { stable_id }))
            if stable_id == "installed-package-lead"
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 1);
    let [record] = snapshot.bundles.as_slice() else {
        panic!("expected exactly one installed bundle");
    };
    assert_eq!(record.bundle_id, "hya/installed-package");
    assert_eq!(record.version, "1.0.0");
    assert_eq!(record.source_digest, [0x66; 32]);
    assert_eq!(record.prepared_digest, old_digest);
    assert_eq!(record.prepared_bytes, old_bytes);
    assert_eq!(record.installed_at, old_installed_at);
}

#[tokio::test]
async fn reserved_builtin_agent_id_is_rejected_and_registry_is_unchanged() {
    // Built-in agents are compiled in, so a bundle may not claim one of their
    // ids. Rejecting at install shows the operator the failure immediately,
    // rather than at the next catalog publish.
    let candidate = installed_candidate(
        "1.0.0",
        "installed package",
        "You are the installed package lead.",
        [0x33; 32],
        1_725_000_003,
    );
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let install = registry
        .install(&["installed-package-lead"], candidate)
        .await;
    assert!(matches!(
        install,
        Err(StoreError::BundleAgentIdReserved { ref agent_id, .. })
            if agent_id == "installed-package-lead"
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn writer_busy_fails_immediately_without_registry_mutation() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };
    let writer = SqliteConnection::connect(&format!("sqlite://{path}")).await;
    let Ok(mut writer) = writer else {
        panic!("writer connection failed: {writer:?}");
    };
    let lock = sqlx::query("BEGIN IMMEDIATE").execute(&mut writer).await;
    let Ok(_) = lock else {
        panic!("writer lock acquisition failed: {lock:?}");
    };

    let install = registry
        .install(
            &[],
            installed_candidate(
                "1.0.0",
                "installed package",
                "You are the installed package lead.",
                [0x44; 32],
                1_725_000_004,
            ),
        )
        .await;

    let rollback = sqlx::query("ROLLBACK").execute(&mut writer).await;
    let Ok(_) = rollback else {
        panic!("writer lock rollback failed: {rollback:?}");
    };
    assert!(matches!(install, Err(StoreError::BundleRegistryBusy)));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn uninstall_removes_active_bundle_and_advances_generation_once() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };
    let installed = registry
        .install(
            &[],
            installed_candidate(
                "1.0.0",
                "installed package",
                "You are the installed package lead.",
                [0x55; 32],
                1_725_000_005,
            ),
        )
        .await;
    let Ok(installed) = installed else {
        panic!("bundle install failed: {installed:?}");
    };
    assert_eq!(installed, BundleInstallOutcome::Installed { generation: 1 });

    let before_uninstall = registry.snapshot().await;
    let Ok(before_uninstall) = before_uninstall else {
        panic!("bundle registry snapshot failed: {before_uninstall:?}");
    };
    assert_eq!(before_uninstall.generation, 1);
    assert_eq!(before_uninstall.bundles.len(), 1);

    let removed = registry.uninstall("hya/installed-package").await;
    let Ok(removed) = removed else {
        panic!("bundle uninstall failed: {removed:?}");
    };
    assert_eq!(removed, BundleUninstallOutcome::Removed { generation: 2 });

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 2);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn uninstall_unknown_bundle_is_typed_not_found_without_generation_change() {
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let uninstall = registry.uninstall("hya/not-installed").await;
    assert!(matches!(
        uninstall,
        Err(StoreError::BundleNotFound { bundle_id }) if bundle_id == "hya/not-installed"
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

#[tokio::test]
async fn uninstall_of_a_never_installed_bundle_is_typed_not_found() {
    // There are no built-in bundles any more, so every uninstall target is
    // either an installed row or simply absent.
    let path = temp_db();
    let registry = BundleRegistry::connect(&path).await;
    let Ok(registry) = registry else {
        panic!("bundle registry connection failed: {registry:?}");
    };

    let uninstall = registry.uninstall("hya/builtin-only").await;
    assert!(matches!(
        uninstall,
        Err(StoreError::BundleNotFound { bundle_id }) if bundle_id == "hya/builtin-only"
    ));

    let snapshot = registry.snapshot().await;
    let Ok(snapshot) = snapshot else {
        panic!("bundle registry snapshot failed: {snapshot:?}");
    };
    assert_eq!(snapshot.generation, 0);
    assert!(snapshot.bundles.is_empty());
}

