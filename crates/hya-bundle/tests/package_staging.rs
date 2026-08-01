use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

use hya_bundle::{PackageInspection, cleanup_orphaned_staging, stage_package};

static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

#[test]
fn staged_public_package_is_private_source_independent_and_cleans_only_its_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_root = std::env::temp_dir().join(format!(
        "hya-bundle-package-staging-{}-{}",
        std::process::id(),
        NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir(&temp_root)?;

    let fixture = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z");
    let source_path = temp_root.join("source.any");
    fs::write(&source_path, fixture.as_slice())?;
    let staging_root = temp_root.join("staging");
    fs::create_dir(&staging_root)?;
    let foreign = staging_root.join("foreign");
    fs::create_dir(&foreign)?;

    let staged = stage_package(&source_path, &staging_root)?;
    fs::remove_file(&source_path)?;

    let children = fs::read_dir(&staging_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let owned_children: Vec<_> = children
        .into_iter()
        .filter(|path| path != &foreign)
        .collect();
    let [owned_dir] = owned_children.as_slice() else {
        panic!("staging root must contain exactly one owned child");
    };
    let owned_metadata = fs::symlink_metadata(owned_dir)?;
    assert!(owned_metadata.file_type().is_dir());
    assert_eq!(owned_metadata.permissions().mode() & 0o777, 0o700);

    let staged_files = fs::read_dir(owned_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let [staged_file] = staged_files.as_slice() else {
        panic!("owned staging directory must contain exactly one file");
    };
    let staged_metadata = fs::symlink_metadata(staged_file)?;
    assert!(staged_metadata.file_type().is_file());
    assert_eq!(staged_metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read(staged_file)?, fixture.as_slice());

    assert!(matches!(staged.inspect()?, PackageInspection::Public(_)));
    assert!(!owned_dir.exists());
    assert!(foreign.is_dir());

    fs::remove_dir_all(&temp_root)?;
    Ok(())
}

#[test]
fn conservative_cleanup_removes_only_unlocked_owned_staging()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_root = std::env::temp_dir().join(format!(
        "hya-bundle-package-staging-{}-{}",
        std::process::id(),
        NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir(&temp_root)?;

    let fixture = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z");
    let source_path = temp_root.join("source.any");
    fs::write(&source_path, fixture.as_slice())?;
    let staging_root = temp_root.join("staging");
    fs::create_dir(&staging_root)?;
    let staged = stage_package(&source_path, &staging_root)?;

    let active_children = fs::read_dir(&staging_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let [active_dir] = active_children.as_slice() else {
        panic!("staging root must contain exactly one active owned directory");
    };
    let active_file = active_dir.join("package");
    assert!(fs::symlink_metadata(&active_file)?.file_type().is_file());

    let orphan_dir = staging_root.join(format!("hya-bundle-stage-{}-orphan", std::process::id()));
    fs::create_dir(&orphan_dir)?;
    fs::set_permissions(&orphan_dir, fs::Permissions::from_mode(0o700))?;
    let orphan_file = orphan_dir.join("package");
    fs::write(&orphan_file, fixture.as_slice())?;
    fs::set_permissions(&orphan_file, fs::Permissions::from_mode(0o600))?;
    let orphan_entries = fs::read_dir(&orphan_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    let [candidate_file] = orphan_entries.as_slice() else {
        panic!("orphan candidate must contain exactly one package file");
    };
    assert_eq!(candidate_file, &orphan_file);
    assert!(fs::symlink_metadata(candidate_file)?.file_type().is_file());

    let foreign = staging_root.join("foreign");
    fs::create_dir(&foreign)?;

    cleanup_orphaned_staging(&staging_root)?;

    assert!(!orphan_dir.exists());
    assert!(active_dir.is_dir());
    assert!(active_file.is_file());
    assert!(foreign.is_dir());

    drop(staged);
    fs::remove_dir_all(&temp_root)?;
    Ok(())
}

#[test]
fn cleanup_missing_staging_root_is_noop() -> Result<(), Box<dyn std::error::Error>> {
    let temp_root = loop {
        let candidate = std::env::temp_dir().join(format!(
            "hya-bundle-package-staging-{}-{}",
            std::process::id(),
            NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed),
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };

    let staging_root = temp_root.join("staging");
    assert!(!staging_root.exists());

    cleanup_orphaned_staging(&staging_root)?;

    assert!(!staging_root.exists());
    fs::remove_dir(&temp_root)?;
    Ok(())
}
