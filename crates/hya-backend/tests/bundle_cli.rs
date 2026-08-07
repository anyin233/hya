//! Integration tests for `hya-backend`: bundle cli.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const LIST_HEADER: &str = "NAME VERSION AGENT STATE";
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

fn bundle_command(data_root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya-backend"));
    command
        .env("XDG_DATA_HOME", data_root)
        .env("HOME", data_root);
    command
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
        Some(vec![BUNDLE_ID, "1.0.0", BUNDLE_AGENT_ID, "active"]),
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
        "prepared_digest=1d36ac29eab07335cdf89dff96a3e142b8fcd3c9ef71d80d9af3a2f043e3aa39",
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
fn bundle_list_is_empty_without_a_registry_and_creates_none()
-> Result<(), Box<dyn std::error::Error>> {
    // Built-in agents are compiled in, not bundles, so a fresh install lists no
    // bundles at all and a read-only command must not create a registry.
    let data_root = unique_data_root()?;
    let registry_path = data_root.join("hya/bundles/registry.sqlite3");
    assert!(!registry_path.exists());

    let list = bundle_command(&data_root)
        .args(["bundle", "list"])
        .output()?;
    assert_success("empty list", &list);
    let list_stdout = String::from_utf8(list.stdout)?;
    assert_eq!(
        list_stdout.lines().collect::<Vec<_>>(),
        vec![LIST_HEADER],
        "unexpected bundle list:\n{list_stdout}"
    );
    assert!(
        !registry_path.exists(),
        "read-only bundle list created a bundle registry"
    );

    let info = bundle_command(&data_root)
        .args(["bundle", "info", "hya/core-agents"])
        .output()?;
    assert!(
        !info.status.success(),
        "info for a never-installed bundle unexpectedly succeeded"
    );
    assert!(
        !registry_path.exists(),
        "read-only bundle info created a bundle registry"
    );

    let uninstall = bundle_command(&data_root)
        .args(["bundle", "uninstall", "hya/core-agents"])
        .output()?;
    assert!(
        !uninstall.status.success(),
        "uninstall of a never-installed bundle unexpectedly succeeded"
    );

    fs::remove_dir_all(&data_root)?;
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
            "prepared_digest: 1d36ac29eab07335cdf89dff96a3e142b8fcd3c9ef71d80d9af3a2f043e3aa39",
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
    assert_eq!(bundle.identity.id, "hya/archive-js");
    assert_eq!(bundle.agent.id.as_str(), "archive-js-lead");
    assert_eq!(bundle.tools.len(), 1);
    assert_eq!(bundle.hooks.len(), 1);
    assert_eq!(bundle.extensions.len(), 1);
    assert_eq!(bundle.tools[0].stable_id, "bundle:hya/archive-js/tool/echo");
    assert_eq!(bundle.tools[0].content, "export const runtime = true;\n");
    assert_eq!(
        bundle.hooks[0].stable_id,
        "bundle:hya/archive-js/hook/event"
    );
    assert_eq!(bundle.hooks[0].content, "export const runtime = true;\n");
    assert_eq!(
        bundle.extensions[0].stable_id,
        "bundle:hya/archive-js/extension/runtime"
    );
    assert_eq!(
        bundle.extensions[0].content,
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
