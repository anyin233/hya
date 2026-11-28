//! `WorkflowCatalog::from_bundles` precedence, revision folding, and
//! multi-root scope rules over prepared bundles.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{
    Bundle, BundleCatalog, BundleSource, PreparedCatalog, SourceFile, prepare_package,
};
use hya_workflow_bundle_core::{
    WorkflowCatalog, WorkflowCatalogOwner, WorkflowCatalogRoots, WorkflowControlError,
};

fn temp_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "hya-workflow-catalog-{label}-{}-{nonce}-{seq}",
        std::process::id()
    ))
}

fn write_workflow(dir: &Path, stem: &str, name: &str, directive: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("{stem}.hya.md")),
        format!(
            "---\nkind: Workflow\nname: {name}\ndescription: {name} fixture.\ninputs:\n  request: Request.\nnodes:\n  execute:\n    agent: hya-task\n    directive: {directive} {{{{input.request}}}}\n---\nflowchart TD\n  execute\n"
        ),
    )
    .unwrap();
}

fn workflow_bundle(bundle_id: &str, workflow_id: &str, prompt: &str) -> BundleSource {
    BundleSource::new(
        bundle_id,
        vec![
            SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nschema_version: 2\nidentity: {{ id: {bundle_id}, version: 1.0.0, publisher: hya }}\nworkflow: {{ id: {workflow_id}, path: workflows/{workflow_id}.hya.md }}\nagents: [{{ id: catalog-worker, role: subagent, prompt: prompts/w.md }}]\n"
                ),
            ),
            SourceFile::new(
                format!("workflows/{workflow_id}.hya.md"),
                format!(
                    "---\nkind: Workflow\nname: {workflow_id}\ndescription: Installed.\nnodes:\n  run: {{ agent: catalog-worker, directive: Run. }}\n---\nflowchart TD\n  run\n"
                ),
            ),
            SourceFile::new("prompts/w.md", prompt),
        ],
    )
}

/// The bundles of `installed` over the first-party `hya/workflow`, in
/// catalog order.
fn bundles(installed: &PreparedCatalog) -> Vec<Bundle> {
    let first_party = hya_bundle::first_party_bundle("hya/workflow").unwrap();
    BundleCatalog::from_verified_catalogs(&[installed, first_party])
        .unwrap()
        .bundles()
        .iter()
        .map(|bundle| (**bundle).clone())
        .collect()
}

#[test]
fn catalog_precedence_and_folded_bundle_revisions() {
    let project = temp_dir("prec-proj");
    let proj_wf = project.join(".hya/workflows");
    let user_wf = temp_dir("prec-user");
    write_workflow(&proj_wf, "collision", "collision", "PROJECT");
    write_workflow(&user_wf, "collision", "collision", "USER");
    let old_bundles =
        bundles(&prepare_package(workflow_bundle("hya/cat-inst", "collision", "OLD")).unwrap());
    let roots = || WorkflowCatalogRoots::new(vec![proj_wf.clone()], Some(user_wf.clone()));

    // Precedence: Project > User > Installed, while qualified refs bypass bare shadowing.
    let cat = WorkflowCatalog::from_bundles(roots(), &old_bundles).unwrap();
    assert!(matches!(
        cat.resolve("collision").unwrap().owner(),
        WorkflowCatalogOwner::Project
    ));
    let inst_ref = "bundle:hya/cat-inst/workflow/collision";
    assert!(matches!(
        cat.resolve(inst_ref).unwrap().owner(),
        WorkflowCatalogOwner::Installed { bundle_id } if bundle_id == "hya/cat-inst"
    ));
    assert!(matches!(
        cat.resolve("plan-impl-review").unwrap().owner(),
        WorkflowCatalogOwner::FirstParty { bundle_id } if bundle_id == "hya/workflow"
    ));

    std::fs::remove_file(proj_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::from_bundles(roots(), &old_bundles)
            .unwrap()
            .resolve("collision")
            .unwrap()
            .owner(),
        WorkflowCatalogOwner::User
    ));
    std::fs::remove_file(user_wf.join("collision.hya.md")).unwrap();
    assert!(matches!(
        WorkflowCatalog::from_bundles(roots(), &old_bundles)
            .unwrap()
            .resolve("collision")
            .unwrap()
            .owner(),
        WorkflowCatalogOwner::Installed { .. }
    ));

    // A bundle agent prompt change folds into the revision.
    let new_bundles =
        bundles(&prepare_package(workflow_bundle("hya/cat-inst", "collision", "NEW")).unwrap());
    let revision = |bundles: &[Bundle]| {
        WorkflowCatalog::from_bundles(roots(), bundles)
            .unwrap()
            .resolve(inst_ref)
            .unwrap()
            .identity()
            .revision
    };
    assert_ne!(revision(&old_bundles), revision(&new_bundles));

    let _ = std::fs::remove_dir_all(project);
    let _ = std::fs::remove_dir_all(user_wf);
}

#[test]
fn multi_root_catalog_dedupes_by_source_and_name_first_root_wins() {
    let root_a = temp_dir("scope-a");
    let root_b = temp_dir("scope-b");
    let wf_a = root_a.join(".hya/workflows");
    let wf_b = root_b.join(".hya/workflows");
    write_workflow(&wf_a, "same", "root-a-same", "A SAME");
    write_workflow(&wf_b, "same", "root-b-same", "B SAME");
    write_workflow(&wf_a, "unique-a", "shared-name", "A SHARED");
    write_workflow(&wf_b, "unique-b", "shared-name", "B SHARED");
    write_workflow(&wf_b, "gamma", "gamma", "GAMMA");

    let cat = WorkflowCatalog::from_bundles(
        WorkflowCatalogRoots::new(vec![wf_a.clone(), wf_b.clone()], None),
        &[],
    )
    .unwrap();
    assert_eq!(
        cat.summaries()
            .into_iter()
            .map(|i| i.name)
            .collect::<Vec<_>>(),
        ["root-a-same", "shared-name", "gamma"]
    );
    for name in ["root-a-same", "shared-name"] {
        assert!(
            cat.resolve(name)
                .unwrap()
                .display_path()
                .starts_with(&wf_a.display().to_string()),
            "{name}"
        );
    }
    assert!(matches!(
        cat.resolve("root-b-same"),
        Err(WorkflowControlError::NotFound { .. })
    ));

    let _ = std::fs::remove_dir_all(root_a);
    let _ = std::fs::remove_dir_all(root_b);
}
