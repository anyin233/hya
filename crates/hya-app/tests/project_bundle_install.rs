//! Integration tests for `hya-app`: installing and removing project-scope
//! bundles under `.hya/bundles` (what `hya bundle install --project` drives).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_app::project_bundles::{
    ProjectBundleError, install_project_bundle, load_project_bundles, plan_project_install,
    project_bundles, remove_project_bundle,
};
use hya_bundle::SourceFile;
use hya_store::{BundleInstallAction, NamespaceInstallPolicy, StoreError};

const DENY: NamespaceInstallPolicy = NamespaceInstallPolicy::DenyConflicts;
const OVERWRITE: NamespaceInstallPolicy = NamespaceInstallPolicy::OverwriteConflicts;

fn temp_project() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch")
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "hya-project-install-{}-{nanos}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create temp project");
    root
}

/// Source files for one AgentBundle with an explicit namespace and prompt.
fn bundle_files(bundle_id: &str, namespace: &str, version: &str, prompt: &str) -> Vec<SourceFile> {
    let agent = bundle_id.replace('/', "-");
    vec![
        SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: AgentBundle\nidentity:\n  id: {bundle_id}\n  version: {version}\n  publisher: hya\nnamespace: {namespace}\nagent:\n  id: {agent}-lead\n  role: main\n  prompt: prompts/lead.md\n  spawn_lifecycle: transient\n"
            ),
        ),
        SourceFile::new("prompts/lead.md", format!("{prompt}\n")),
    ]
}

fn project_ids(dir: &Path) -> Vec<String> {
    project_bundles(dir)
        .iter()
        .map(|bundle| bundle.bundle_id().to_string())
        .collect()
}

#[test]
fn install_writes_a_loadable_source_directory_and_reinstall_is_unchanged() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    let files = bundle_files("acme/tools", "tools", "1.0.0", "Lead.");

    let plan = plan_project_install(&dir, &files, &[], DENY).expect("plan fresh install");
    assert_eq!(plan.action, BundleInstallAction::Install);
    assert_eq!(plan.target, dir.join("acme__tools"));
    assert!(
        !dir.exists(),
        "planning must not create the project directory"
    );

    let done = install_project_bundle(&dir, files.clone(), &[], DENY).expect("install");
    assert_eq!(done.action, BundleInstallAction::Install);
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Lead.\n"
    );
    let (catalogs, _) = load_project_bundles(&dir);
    assert_eq!(catalogs.len(), 1, "the runtime loader must see the bundle");
    assert_eq!(project_ids(&dir), vec!["acme/tools".to_string()]);
    let leftovers = std::fs::read_dir(root.join(".hya"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        leftovers,
        vec!["bundles".to_string()],
        "no staging leftovers"
    );

    let again = install_project_bundle(&dir, files, &[], DENY).expect("reinstall");
    assert_eq!(again.action, BundleInstallAction::Unchanged);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn upgrade_replaces_in_place_and_downgrade_or_same_version_edit_need_overwrite() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    install_project_bundle(
        &dir,
        bundle_files("acme/tools", "tools", "1.0.0", "One."),
        &[],
        DENY,
    )
    .expect("install v1");

    let upgraded = install_project_bundle(
        &dir,
        bundle_files("acme/tools", "tools", "2.0.0", "Two."),
        &[],
        DENY,
    )
    .expect("upgrade");
    assert_eq!(
        upgraded.action,
        BundleInstallAction::Replace {
            installed_version: "1.0.0".to_string()
        }
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Two.\n"
    );

    let downgrade = bundle_files("acme/tools", "tools", "1.0.0", "One.");
    assert!(matches!(
        plan_project_install(&dir, &downgrade, &[], DENY),
        Err(ProjectBundleError::Store(
            StoreError::BundleDowngradeRequired { .. }
        ))
    ));
    let edited = bundle_files("acme/tools", "tools", "2.0.0", "Edited.");
    assert!(matches!(
        plan_project_install(&dir, &edited, &[], DENY),
        Err(ProjectBundleError::Store(
            StoreError::BundleContentConflict { .. }
        ))
    ));
    install_project_bundle(&dir, edited, &[], OVERWRITE).expect("overwrite same version");
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Edited.\n"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn namespace_takeover_needs_overwrite_and_removes_the_incumbent() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    install_project_bundle(
        &dir,
        bundle_files("acme/tools", "shared", "1.0.0", "Tools."),
        &[],
        DENY,
    )
    .expect("install incumbent");
    let rival = bundle_files("acme/rival", "shared", "1.0.0", "Rival.");

    assert!(matches!(
        plan_project_install(&dir, &rival, &[], DENY),
        Err(ProjectBundleError::Store(
            StoreError::NamespaceConflict { .. }
        ))
    ));
    let plan = install_project_bundle(&dir, rival, &[], OVERWRITE).expect("take over namespace");
    assert_eq!(plan.displaced.len(), 1);
    assert_eq!(plan.displaced[0].bundle_id(), "acme/tools");
    assert_eq!(project_ids(&dir), vec!["acme/rival".to_string()]);
    assert!(!dir.join("acme__tools").exists());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn reserved_agent_ids_and_foreign_directories_are_refused() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    let files = bundle_files("acme/tools", "tools", "1.0.0", "Lead.");
    assert!(matches!(
        plan_project_install(&dir, &files, &["acme-tools-lead"], DENY),
        Err(ProjectBundleError::Store(
            StoreError::BundleAgentIdReserved { .. }
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
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn remove_deletes_the_bundle_directory_found_by_id() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    // A hand-authored directory whose name does not follow the install layout.
    let custom = dir.join("my-tools");
    std::fs::create_dir_all(custom.join("prompts")).unwrap();
    for file in bundle_files("acme/tools", "tools", "1.0.0", "Lead.") {
        std::fs::write(custom.join(file.path()), file.bytes()).unwrap();
    }
    assert_eq!(project_ids(&dir), vec!["acme/tools".to_string()]);

    assert!(matches!(
        remove_project_bundle(&dir, "acme/missing"),
        Err(ProjectBundleError::Store(StoreError::BundleNotFound { .. }))
    ));
    let removed = remove_project_bundle(&dir, "acme/tools").expect("remove");
    assert_eq!(removed.dir(), custom.as_path());
    assert!(!custom.exists());
    assert!(project_ids(&dir).is_empty());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn bundle_config_file_survives_upgrade_and_is_not_bundle_content() {
    let root = temp_project();
    let dir = root.join(".hya/bundles");
    install_project_bundle(
        &dir,
        bundle_files("acme/tools", "tools", "1.0.0", "One."),
        &[],
        DENY,
    )
    .expect("install v1");
    let (before_catalogs, before_fingerprint) = load_project_bundles(&dir);
    let config = dir.join("acme__tools/config.yml");
    std::fs::write(
        &config,
        "agents:\n  acme-tools-lead:\n    model: p/m\nkey: kept\n",
    )
    .unwrap();
    std::fs::write(dir.join("acme__tools/.config.yml.lock"), "").unwrap();

    let (with_config, with_config_fingerprint) = load_project_bundles(&dir);
    assert_eq!(
        with_config_fingerprint, before_fingerprint,
        "config.yml must not change the project content fingerprint"
    );
    assert_eq!(with_config[0].digest(), before_catalogs[0].digest());
    let unchanged = install_project_bundle(
        &dir,
        bundle_files("acme/tools", "tools", "1.0.0", "One."),
        &[],
        DENY,
    )
    .expect("same-content reinstall");
    assert_eq!(unchanged.action, BundleInstallAction::Unchanged);

    let mut incoming = bundle_files("acme/tools", "tools", "2.0.0", "Two.");
    incoming.push(SourceFile::new("config.yml", "shipped: overwrite\n"));
    let upgraded = install_project_bundle(&dir, incoming, &[], DENY).expect("upgrade");
    assert!(matches!(
        upgraded.action,
        BundleInstallAction::Replace { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "agents:\n  acme-tools-lead:\n    model: p/m\nkey: kept\n",
        "an upgrade must preserve the user's config.yml"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("acme__tools/prompts/lead.md")).unwrap(),
        "Two.\n"
    );

    let fresh = root.join(".hya/other");
    let mut shipped = bundle_files("acme/fresh", "fresh", "1.0.0", "Fresh.");
    shipped.push(SourceFile::new("config.yml", "shipped: ignored\n"));
    install_project_bundle(&fresh, shipped, &[], DENY).expect("fresh install");
    assert!(
        !fresh.join("acme__fresh/config.yml").exists(),
        "incoming sources never write the user-owned config.yml"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
