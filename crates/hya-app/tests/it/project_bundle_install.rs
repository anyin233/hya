//! Integration tests for `hya-app`: installing and removing project-scope
//! bundles under `.hya/bundles`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::bundle_registry::{BundleInstallAction, BundleRegistryError, NamespaceInstallPolicy};
use hya_app::project_bundles::{
    ProjectBundleError, install_project_bundle, load_project_bundles, plan_project_install,
    project_bundles, remove_project_bundle,
};
use hya_bundle::SourceFile;
use hya_engine::RuntimeCatalogRefresh as _;

const DENY: NamespaceInstallPolicy = NamespaceInstallPolicy::DenyConflicts;
const OVERWRITE: NamespaceInstallPolicy = NamespaceInstallPolicy::OverwriteConflicts;

fn temp_project() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "hya-project-install-{}-{nanos}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn bundle_files(bundle_id: &str, namespace: &str, version: &str, prompt: &str) -> Vec<SourceFile> {
    let agent = bundle_id.replace('/', "-");
    vec![
        SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: Bundle\nidentity: {{ id: {bundle_id}, version: {version}, publisher: hya }}\nnamespace: {namespace}\nagents: [{{ id: {agent}-lead, role: main, prompt: prompts/lead.md }}]\n"
            ),
        ),
        SourceFile::new("prompts/lead.md", format!("{prompt}\n")),
    ]
}

fn project_ids(dir: &Path) -> Vec<String> {
    project_bundles(dir)
        .into_iter()
        .map(|b| b.unwrap().bundle_id().to_string())
        .collect()
}

#[test]
fn install_upgrade_conflicts_namespace_takeover_and_config_preservation() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    let v1 = bundle_files("acme/tools", "shared", "1.0.0", "One.");

    // Plan does not create directories; install writes cleanly with no staging leftovers.
    let plan = plan_project_install(&dir, &v1, &[], DENY).unwrap();
    assert_eq!(plan.action, BundleInstallAction::Install);
    assert_eq!(plan.target, dir.join("acme__tools"));
    assert!(!dir.exists());

    assert_eq!(
        install_project_bundle(&dir, v1.clone(), &[], DENY)
            .unwrap()
            .action,
        BundleInstallAction::Install
    );
    assert_eq!(project_ids(&dir), vec!["acme/tools".to_string()]);
    assert_eq!(
        std::fs::read_dir(root.join(".hya"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["bundles".to_string()]
    );

    // User config.yml is excluded from bundle fingerprint and preserved across upgrades.
    let (before_cat, before_fp) = load_project_bundles(&dir).unwrap();
    let cfg = dir.join("acme__tools/config.yml");
    std::fs::write(&cfg, "key: kept\n").unwrap();
    std::fs::write(dir.join("acme__tools/.config.yml.lock"), "").unwrap();
    let (with_cfg, with_cfg_fp) = load_project_bundles(&dir).unwrap();
    assert_eq!(with_cfg_fp, before_fp);
    assert_eq!(with_cfg[0].digest(), before_cat[0].digest());
    assert_eq!(
        install_project_bundle(&dir, v1, &[], DENY).unwrap().action,
        BundleInstallAction::Unchanged
    );

    let mut v2 = bundle_files("acme/tools", "shared", "2.0.0", "Two.");
    v2.push(SourceFile::new("config.yml", "shipped: ignored\n"));
    assert_eq!(
        install_project_bundle(&dir, v2, &[], DENY).unwrap().action,
        BundleInstallAction::Replace {
            installed_version: "1.0.0".into(),
        }
    );
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "key: kept\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Two.\n"
    );

    // Downgrade and same-version content edit require overwrite.
    let v1_down = bundle_files("acme/tools", "shared", "1.0.0", "One.");
    assert!(matches!(
        plan_project_install(&dir, &v1_down, &[], DENY),
        Err(ProjectBundleError::Registry(
            BundleRegistryError::BundleDowngradeRequired { .. }
        ))
    ));
    let v2_edit = bundle_files("acme/tools", "shared", "2.0.0", "Edited.");
    assert!(matches!(
        plan_project_install(&dir, &v2_edit, &[], DENY),
        Err(ProjectBundleError::Registry(
            BundleRegistryError::BundleContentConflict { .. }
        ))
    ));
    install_project_bundle(&dir, v2_edit, &[], OVERWRITE).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Edited.\n"
    );

    // Namespace takeover displaces incumbent directory under Overwrite.
    let rival = bundle_files("acme/rival", "shared", "1.0.0", "Rival.");
    assert!(matches!(
        plan_project_install(&dir, &rival, &[], DENY),
        Err(ProjectBundleError::Registry(
            BundleRegistryError::NamespaceConflict { .. }
        ))
    ));
    let takeover = install_project_bundle(&dir, rival, &[], OVERWRITE).unwrap();
    assert_eq!(takeover.displaced.len(), 1);
    assert_eq!(takeover.displaced[0].bundle_id(), "acme/tools");
    assert_eq!(project_ids(&dir), vec!["acme/rival".to_string()]);
    assert!(!dir.join("acme__tools").exists());

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn reserved_agent_occupied_dir_and_remove_by_id() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    let files = bundle_files("acme/tools", "tools", "1.0.0", "Lead.");

    assert!(matches!(
        plan_project_install(&dir, &files, &["acme-tools-lead"], DENY),
        Err(ProjectBundleError::Registry(
            BundleRegistryError::BundleAgentIdReserved { .. }
        ))
    ));

    std::fs::create_dir_all(dir.join("acme__tools")).unwrap();
    std::fs::write(dir.join("acme__tools/notes.txt"), "hand written").unwrap();
    assert!(matches!(
        plan_project_install(&dir, &files, &[], OVERWRITE),
        Err(ProjectBundleError::DirectoryOccupied { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/notes.txt")).unwrap(),
        "hand written"
    );
    std::fs::remove_dir_all(dir.join("acme__tools")).unwrap();

    // Hand-authored custom directory is found and deleted by bundle ID.
    let custom = dir.join("my-tools");
    std::fs::create_dir_all(custom.join("prompts")).unwrap();
    for file in files {
        std::fs::write(custom.join(file.path()), file.bytes()).unwrap();
    }
    assert!(matches!(
        remove_project_bundle(&dir, "acme/missing"),
        Err(ProjectBundleError::Registry(
            BundleRegistryError::BundleNotFound { .. }
        ))
    ));
    let removed = remove_project_bundle(&dir, "acme/tools").unwrap();
    assert_eq!(removed.dir(), custom.as_path());
    assert!(!custom.exists());
    assert!(project_ids(&dir).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn installed_project_bundle_reaches_only_its_project_scope() {
    let root = temp_project();
    let other = temp_project();
    let dir = root.join(".hya/bundles");
    install_project_bundle(
        &dir,
        bundle_files("acme/tools", "tools", "1.0.0", "Lead."),
        &[],
        DENY,
    )
    .unwrap();

    let runtime = Arc::new(hya_engine::RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        hya_app::builtin_agent_catalog().unwrap(),
    ));
    let refresh = hya_app::ProjectScopeRefresh::new(Arc::new(
        hya_app::InstalledBundleRefresh::new(root.join("registry.db")),
    ));
    let scope = hya_engine::CatalogScope::Project {
        id: hya_proto::ProjectId::new(),
        roots: vec![other.clone(), root.clone()],
    };
    refresh.refresh_if_changed(&runtime).await.unwrap();
    assert!(refresh.refresh_scope(&runtime, &scope).await.unwrap());
    assert!(
        runtime
            .bind_scoped(&scope, &root)
            .unwrap()
            .resolve_agent("acme-tools-lead")
            .is_some()
    );
    assert!(
        runtime
            .bind_turn(&root)
            .unwrap()
            .resolve_agent("acme-tools-lead")
            .is_none()
    );

    remove_project_bundle(&dir, "acme/tools").unwrap();
    assert!(
        refresh
            .refresh_scope_explicit(&runtime, &scope)
            .await
            .unwrap()
    );
    assert!(
        runtime
            .bind_scoped(&scope, &root)
            .unwrap()
            .resolve_agent("acme-tools-lead")
            .is_none()
    );
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}
