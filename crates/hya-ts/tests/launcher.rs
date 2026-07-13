#![allow(clippy::unwrap_used)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use hya_ts::{Cli, build_bun_command, resolve_runtime_dir};

#[test]
fn parses_public_launcher_contract_and_builds_tui_command() {
    let temp = temp_dir("hya-ts-command");
    let project = temp.join("project");
    let runtime = temp.join("runtime");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();

    let cli = Cli::try_parse_from([
        "hya-ts",
        project.to_str().unwrap(),
        "--server",
        "http://127.0.0.1:9876",
        "--backend-bin",
        "/tmp/hya-backend-test",
        "--bun",
        "/tmp/bun-test",
        "--continue",
        "--session",
        "ses_123",
        "--fork",
        "--prompt",
        "hello",
        "--agent",
        "build",
        "--model",
        "provider/model",
    ])
    .unwrap();
    cli.validate().unwrap();

    let command = build_bun_command(&cli, &runtime).unwrap();
    assert_eq!(command.program, PathBuf::from("/tmp/bun-test"));
    assert_eq!(command.current_dir, runtime.canonicalize().unwrap());
    assert_eq!(
        command.args,
        os_strings(&[
            "src/main.tsx",
            "--url",
            "http://127.0.0.1:9876",
            "--project",
            project.canonicalize().unwrap().to_str().unwrap(),
            "--continue",
            "--session",
            "ses_123",
            "--fork",
            "--prompt",
            "hello",
            "--agent",
            "build",
            "--model",
            "provider/model",
        ])
    );
}

#[test]
fn defaults_to_canonical_current_project() {
    let project = temp_dir("hya-ts-default-project");
    let runtime = temp_dir("hya-ts-default-runtime");
    let cli = Cli::try_parse_from(["hya-ts", "--server", "https://hya.example"]).unwrap();

    let command = hya_ts::build_bun_command_from(&cli, &runtime, &project).unwrap();

    assert_eq!(
        command.args,
        os_strings(&[
            "src/main.tsx",
            "--url",
            "https://hya.example",
            "--project",
            project.canonicalize().unwrap().to_str().unwrap(),
        ])
    );
}

#[test]
fn validates_url_and_fork_before_process_construction() {
    assert!(Cli::try_parse_from(["hya-ts", "--server", "not-a-url"]).is_err());

    let fork_only = Cli::try_parse_from(["hya-ts", "--fork"]).unwrap();
    assert_eq!(
        fork_only.validate().unwrap_err(),
        "--fork requires --continue or --session"
    );

    Cli::try_parse_from(["hya-ts", "--fork", "--session", "ses_123"])
        .unwrap()
        .validate()
        .unwrap();
}

#[test]
fn resolves_runtime_override_then_installed_sibling_then_workspace() {
    let root = temp_dir("hya-ts-runtime-resolution");
    let explicit = root.join("explicit");
    let installed = root.join("prefix/lib/hya/hya-tui-ts");
    let workspace = root.join("workspace/packages/hya-tui-ts");
    for dir in [&explicit, &installed, &workspace] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let exe = root.join("prefix/bin/hya-ts");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, []).unwrap();

    assert_eq!(
        resolve_runtime_dir(Some(explicit.as_os_str()), &exe, &root.join("workspace")).unwrap(),
        explicit.canonicalize().unwrap()
    );
    assert_eq!(
        resolve_runtime_dir(None, &exe, &root.join("workspace")).unwrap(),
        installed.canonicalize().unwrap()
    );
    std::fs::remove_dir_all(&installed).unwrap();
    assert_eq!(
        resolve_runtime_dir(None, &exe, &root.join("workspace")).unwrap(),
        workspace.canonicalize().unwrap()
    );
}

fn os_strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn temp_dir(prefix: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    dir
}
