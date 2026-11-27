//! Real-binary lifecycle integration tests for `hya bundle`.
//! Pure formatting, suffix/envelope validation, search filtering, schema rows,
//! and confirmation prompts live in `src/bundle_cmd.rs` unit tests.

use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hya_bundle::{BundleSource, SourceFile, write_public_package};

use crate::common::{Scratch, TestResult, assert_success, hya_bin};

const LIST_HEADER: &str = "NAME VERSION AGENT STATE KIND WORKFLOW SCOPE";
const BUNDLE_ID: &str = "hya/valid-public";

fn bundle_cmd(root: &Path) -> Command {
    let mut cmd = Command::new(hya_bin());
    cmd.env("XDG_DATA_HOME", root)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("HOME", root)
        .current_dir(root);
    cmd
}

fn run(root: &Path, args: &[&str]) -> Result<Output, std::io::Error> {
    bundle_cmd(root).arg("bundle").args(args).output()
}

fn ok(label: &str, root: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let out = run(root, args)?;
    assert_success(label, &out);
    Ok(String::from_utf8(out.stdout)?)
}

fn write_pkg(
    root: &Path,
    name: &str,
    files: Vec<(&str, &[u8])>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let src = BundleSource::new(
        name,
        files
            .into_iter()
            .map(|(p, b)| SourceFile::new(p, b.to_vec()))
            .collect(),
    );
    let path = root.join(format!("{name}.hyabundle"));
    fs::write(&path, write_public_package(&src)?)?;
    Ok(path)
}

fn write_fixture(root: &Path) -> Result<PathBuf, std::io::Error> {
    let pkg = root.join("demo.hyabundle");
    fs::write(
        &pkg,
        include_bytes!("../../../hya-bundle/tests/fixtures/packages/valid_public_bundle_copy.7z"),
    )?;
    Ok(pkg)
}

fn checked_pkg(
    root: &Path,
    name: &str,
    check: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let yaml = format!(
        "schema_version: 2\nkind: Bundle\nidentity: {{ id: hya/{name}, version: 1.0.0, publisher: hya }}\ncheck: {{ command: {check}, timeout_secs: 30 }}\nagents: [{{ id: {name}, role: subagent, prompt: prompts/p.md }}]\n"
    );
    write_pkg(
        root,
        name,
        vec![
            ("bundle.yaml", yaml.as_bytes()),
            ("prompts/p.md", b"Prove yourself.\n"),
        ],
    )
}

fn with_stdin(
    root: &Path,
    args: &[&OsStr],
    answer: Option<&str>,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = bundle_cmd(root)
        .arg("bundle")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("child stdin missing")?;
    if let Some(ans) = answer {
        stdin.write_all(ans.as_bytes())?;
    }
    drop(stdin);
    Ok(child.wait_with_output()?)
}

struct BundleDaemon {
    root: PathBuf,
    db: String,
    url: String,
}

impl BundleDaemon {
    fn start(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let db = root.join("s.db").to_string_lossy().into_owned();
        let out = bundle_cmd(root)
            .args(["serve", "start", "--json", "--db", &db])
            .output()?;
        assert_success("serve start", &out);
        let disc: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
            url: disc["url"].as_str().ok_or("daemon URL")?.into(),
        })
    }

    fn cmd(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        bundle_cmd(&self.root)
            .arg("bundle")
            .args(args)
            .args(["--db", &self.db])
            .output()
    }
}

impl Drop for BundleDaemon {
    fn drop(&mut self) {
        let _ = bundle_cmd(&self.root)
            .args(["serve", "stop", "--db", &self.db])
            .output();
    }
}

#[test]
fn bundle_user_and_project_lifecycle_verify_install_list_info_search_schema_remove() -> TestResult {
    let scratch = Scratch::new("hya-bundle-lifecycle")?;
    let root = &scratch.0;
    let reg = root.join("hya/bundles/registry.sqlite3");
    let pkg = write_pkg(
        root,
        "schema-cli",
        vec![
            (
                "bundle.yaml",
                b"schema_version: 2\nkind: Bundle\nidentity: { id: hya/schema-cli, version: 1.0.0, publisher: hya }\nschemas: [{ scheme: db, tool: query, writable: false }]\nresources: { tools: [{ id: query, path: extensions/runtime.js }] }\nruntime: { kind: js, mode: managed, entry: extensions/runtime.js }\nagents: [{ id: lead, role: main, resource_view: { allow: [query, runtime] } }]\n",
            ),
            ("extensions/runtime.js", b"export default {}"),
        ],
    )?;
    let pkg_s = pkg.to_str().ok_or("pkg")?;

    // Read-only verify/list/info/schema never create a registry.
    let v = ok("verify", root, &["verify", pkg_s])?;
    assert!(
        v.contains("verified hya/schema-cli 1.0.0")
            && v.contains("action=install")
            && !reg.exists()
    );
    let list = ok("list", root, &["list"])?;
    assert!(
        list.starts_with(LIST_HEADER) && list.contains("hya/plan-impl-review") && !reg.exists()
    );
    let fp = ok("fp info", root, &["info", "hya/plan-impl-review"])?;
    assert!(fp.contains("origin=first-party") && fp.contains("immutable=true"));
    let fp_rm = run(root, &["uninstall", "-y", "hya/plan-impl-review"])?;
    assert!(
        !fp_rm.status.success()
            && String::from_utf8_lossy(&fp_rm.stderr).contains("immutable first-party")
            && !reg.exists()
    );

    // User-scope install -> info -> search -> schema -> uninstall.
    let inst = ok("install user", root, &["install", "-y", pkg_s])?;
    assert!(
        inst.contains("hya/schema-cli")
            && inst.contains("generation=1")
            && inst.contains("self-check: none declared")
    );
    let info = ok("info user", root, &["info", "hya/schema-cli"])?;
    for n in [
        "name=hya/schema-cli",
        "origin=installed",
        "scope=user",
        "schema=db tool=query writable=false",
    ] {
        assert!(info.contains(n), "missing {n}:\n{info}");
    }
    assert!(
        ok("search", root, &["search", "SCHEMA-CLI"])?
            .contains("hya/schema-cli 1.0.0 lead active Bundle - user")
    );
    assert_eq!(
        ok("schema", root, &["schema", "hya/schema-cli"])?,
        "SCHEME TOOL WRITABLE\ndb query false\n"
    );
    assert!(
        ok(
            "uninstall user",
            root,
            &["uninstall", "-y", "hya/schema-cli"]
        )?
        .contains("generation=2")
    );
    let gone = run(root, &["schema", "hya/schema-cli"])?;
    assert!(
        !gone.status.success()
            && String::from_utf8_lossy(&gone.stderr).contains("BUNDLE_NOT_FOUND")
    );

    // Project-scope install -> list --project -> info -> remove --project.
    assert!(
        ok(
            "install project",
            root,
            &["install", "--project", "-y", pkg_s]
        )?
        .contains("scope=project")
    );
    let bundle_dir = root.join(".hya/bundles/hya__schema-cli");
    assert!(bundle_dir.join("bundle.yaml").is_file());
    let p_list = ok("list project", root, &["list", "--project"])?;
    assert!(
        p_list.lines().count() == 2
            && p_list.contains("hya/schema-cli")
            && p_list.contains(" project")
    );
    assert!(ok("info project", root, &["info", "hya/schema-cli"])?.contains("scope=project"));
    assert!(
        !run(root, &["info", "--user", "hya/schema-cli"])?
            .status
            .success()
    );
    ok(
        "remove project",
        root,
        &["remove", "--project", "-y", "hya/schema-cli"],
    )?;
    assert!(!bundle_dir.exists());
    Ok(())
}

#[test]
fn bundle_install_and_remove_confirmation_prompt_wiring() -> TestResult {
    let scratch = Scratch::new("hya-bundle-confirm")?;
    let root = &scratch.0;
    let pkg = write_fixture(root)?;
    let install_args = [OsStr::new("install"), pkg.as_os_str()];
    for ans in [Some("n\n"), None] {
        let out = with_stdin(root, &install_args, ans)?;
        let err = String::from_utf8(out.stderr)?;
        assert!(
            !out.status.success()
                && err.contains(BUNDLE_ID)
                && err.contains("[y/N]")
                && err.contains("cancelled")
        );
    }
    let accepted = with_stdin(root, &install_args, Some("y\n"))?;
    assert_success("confirmed install", &accepted);
    assert!(String::from_utf8(accepted.stdout)?.contains("scope=user"));

    let declined = with_stdin(
        root,
        &[OsStr::new("remove"), OsStr::new(BUNDLE_ID)],
        Some("no\n"),
    )?;
    assert!(!declined.status.success() && String::from_utf8(declined.stderr)?.contains("[y/N]"));
    let silent = with_stdin(
        root,
        &[
            OsStr::new("uninstall"),
            OsStr::new("-y"),
            OsStr::new(BUNDLE_ID),
        ],
        None,
    )?;
    assert_success("uninstall -y", &silent);
    assert!(!String::from_utf8(silent.stderr)?.contains("[y/N]"));
    Ok(())
}

#[test]
fn bundle_self_check_passes_and_failing_check_refuses_install() -> TestResult {
    let scratch = Scratch::new("hya-bundle-check")?;
    let root = &scratch.0;
    let broken = checked_pkg(
        root,
        "self-broken",
        "[sh, -c, 'echo tests-are-red >&2; exit 4']",
    )?;
    let broken_s = broken.to_str().ok_or("broken")?;
    for args in [&["verify", broken_s][..], &["install", "-y", broken_s][..]] {
        let out = run(root, args)?;
        let err = String::from_utf8(out.stderr)?;
        assert!(
            !out.status.success()
                && err.contains("tests-are-red")
                && err.contains("nothing was installed")
        );
    }
    assert!(!root.join("hya/bundles/registry.sqlite3").exists());

    let proven = checked_pkg(
        root,
        "self-proven",
        r#"[sh, -c, 'test -f prompts/p.md && test -f "$HYA_BUNDLE_ROOT/bundle.yaml" && echo proven-$HYA_BUNDLE_ID']"#,
    )?;
    let stdout = ok(
        "install self-proven",
        root,
        &["install", "-y", proven.to_str().ok_or("proven")?],
    )?;
    assert!(
        stdout.contains("self-check: passed") && stdout.contains("activation: no backend runs")
    );
    Ok(())
}

#[test]
fn bundle_first_party_collision_and_override_fallback() -> TestResult {
    let scratch = Scratch::new("hya-bundle-fp")?;
    let root = &scratch.0;
    let collision = write_pkg(
        root,
        "collision",
        vec![
            ("bundle.yaml", b"schema_version: 2\nkind: Bundle\nidentity: { id: hya/core-agents, version: 9.9.9, publisher: attacker }\nagents: [{ id: collision-agent, role: main, prompt: p.md }]\n"),
            ("p.md", b"nope\n"),
        ],
    )?;
    let rej = run(root, &["install", "-y", collision.to_str().ok_or("c")?])?;
    assert!(
        !rej.status.success()
            && String::from_utf8(rej.stderr)?
                .contains("immutable trusted preset `hya/core-agents`")
    );
    assert!(!root.join("hya/bundles/registry.sqlite3").exists());

    let over = write_pkg(
        root,
        "goal-loop-override",
        vec![
            ("bundle.yaml", b"schema_version: 2\nkind: Bundle\nidentity: { id: hya/goal-loop, version: 9.0.0, publisher: local }\nnamespace: goal-loop\nresources: { skills: [{ id: evaluator-prompt, path: evaluator.md }] }\n"),
            ("evaluator.md", b"---\nname: evaluator-prompt\ndescription: override\n---\nCLI_OVERRIDE\n"),
        ],
    )?;
    ok(
        "install override",
        root,
        &["install", "-y", over.to_str().ok_or("o")?],
    )?;
    let info = ok("info override", root, &["info", "hya/goal-loop"])?;
    assert!(
        info.contains("version=9.0.0\n") && info.contains("origin=installed\n"),
        "{info}"
    );
    ok(
        "uninstall override",
        root,
        &["uninstall", "-y", "hya/goal-loop"],
    )?;
    assert!(
        ok("info fallback", root, &["info", "hya/goal-loop"])?.contains("origin=first-party\n")
    );
    Ok(())
}

#[test]
fn bundle_install_claude_translates_and_installs_fixture() -> TestResult {
    if !Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return Ok(());
    }
    let scratch = Scratch::new("hya-bundle-claude")?;
    let root = &scratch.0;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-plugin");
    let fix = fixture.to_str().ok_or("fixture")?;
    assert!(
        ok("claude install", root, &["install", "-y", "--claude", fix])?
            .contains("installed claude/demo 1.0.0")
    );
    assert!(
        ok(
            "claude reinstall",
            root,
            &["install", "-y", "--claude", fix]
        )?
        .contains("unchanged claude/demo")
    );
    let info = ok("claude info", root, &["info", "claude/demo"])?;
    for expected in [
        "name=claude/demo",
        "agent=reviewer",
        "skill=bundle:claude/demo/skill/audit",
        "mcp=bundle:claude/demo/mcp/vecdb",
    ] {
        assert!(
            info.lines().any(|l| l == expected),
            "missing {expected}:\n{info}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn bundle_live_daemon_activation_enable_disable_and_unreadable_isolation() -> TestResult {
    let scratch = Scratch::new("hya-bundle-live")?;
    let root = &scratch.0;
    let pkg = write_fixture(root)?;
    let pkg_s = pkg.to_str().ok_or("pkg")?;
    ok("install valid", root, &["install", "-y", pkg_s])?;
    let corrupt = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,json,hashlib,sys\nwith sqlite3.connect(sys.argv[1]) as db:\n row=db.execute('SELECT prepared_bytes FROM installed_bundle WHERE bundle_id=?',(sys.argv[2],)).fetchone()\n doc=json.loads(row[0]);doc['format_version']=3\n data=json.dumps(doc).encode()\n db.execute('UPDATE installed_bundle SET prepared_bytes=?,prepared_digest=? WHERE bundle_id=?',(data,hashlib.sha256(data).hexdigest(),sys.argv[2]))",
        ])
        .arg(root.join("hya/bundles/registry.sqlite3"))
        .arg(BUNDLE_ID)
        .output()?;
    assert_success("corrupt registry row", &corrupt);

    let daemon = BundleDaemon::start(root)?;
    let refresh: hya_api::v1::RefreshBundlesResponse = reqwest::Client::new()
        .post(format!("{}/v1/bundles:refresh", daemon.url))
        .json(&serde_json::json!({}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(refresh.errors.len() == 1 && refresh.errors[0].bundle_id == BUNDLE_ID);

    let info = run(root, &["info", BUNDLE_ID])?;
    assert!(!info.status.success() && String::from_utf8_lossy(&info.stderr).contains("unreadable"));

    // Disable stops refresh errors; enable fails loudly; disabled reinstall stays disabled.
    assert_success("disable unreadable", &daemon.cmd(&["disable", BUNDLE_ID])?);
    let enabled = daemon.cmd(&["enable", BUNDLE_ID])?;
    assert!(
        !enabled.status.success() && String::from_utf8_lossy(&enabled.stderr).contains(BUNDLE_ID)
    );
    assert_success("re-disable", &daemon.cmd(&["disable", BUNDLE_ID])?);
    let reinstall = daemon.cmd(&["install", "-y", pkg_s])?;
    assert_success("reinstall disabled unreadable", &reinstall);
    assert!(String::from_utf8(reinstall.stdout)?.contains("installed, disabled"));
    assert_success(
        "uninstall disabled",
        &daemon.cmd(&["uninstall", "-y", BUNDLE_ID])?,
    );

    // Dead-process bundle fails activation with attribution while a healthy bundle still activates.
    let dead = write_pkg(
        root,
        "dead-process",
        vec![("bundle.yaml", b"schema_version: 2\nkind: Bundle\nidentity: { id: acme/dead-process, version: 1.0.0, publisher: acme }\nruntime: { kind: js, mode: process, command: [/nonexistent/hya-bundle-test-process] }\n")],
    )?;
    let failed = daemon.cmd(&["install", "-y", dead.to_str().ok_or("dead")?])?;
    let fail_err = String::from_utf8(failed.stderr)?;
    assert!(
        !failed.status.success()
            && fail_err.contains("installed but not activated")
            && fail_err.contains("acme/dead-process")
    );

    let healthy = checked_pkg(root, "healthy-live", "[true]")?;
    let installed = daemon.cmd(&["install", "-y", healthy.to_str().ok_or("healthy")?])?;
    assert_success("healthy install", &installed);
    assert!(String::from_utf8(installed.stdout)?.contains("activation: active in the backend"));
    assert!(String::from_utf8(installed.stderr)?.contains("acme/dead-process"));
    Ok(())
}
