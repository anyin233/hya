//! A user-installed native bundle contributes `hya` commands routed to its
//! driver and to its BundleApi, through the same path as built-in bundles,
//! and keeps them under `--bare` (ADR-0040). Its `database: true` commands
//! run on `--db`, in process or through the server that owns it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{
    ForegroundServer, Scratch, TestResult, assert_success, combined_text, hya_bin,
};

const LIB: &str = "libhya_cli_driver_example.so";

/// The example cdylibs Cargo built beside `hya` (`deps/`, then build units
/// newest first). Each Cargo invocation may build its own unit, and only one
/// that `hya`'s invocation built carries `hya`'s ABI digest.
fn example_libraries() -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let bin = PathBuf::from(hya_bin());
    let debug = bin.parent().ok_or("hya binary has no parent")?;
    let mut units = std::fs::read_dir(debug.join("build/hya-cli-driver-example"))
        .into_iter()
        .flatten()
        .filter_map(|unit| {
            let lib = unit.ok()?.path().join("out").join(LIB);
            Some((lib.metadata().ok()?.modified().ok()?, lib))
        })
        .collect::<Vec<_>>();
    units.sort_by(|left, right| right.cmp(left));
    let deps = debug.join("deps").join(LIB);
    let libraries = deps
        .is_file()
        .then_some(deps)
        .into_iter()
        .chain(units.into_iter().map(|(_, lib)| lib))
        .collect::<Vec<_>>();
    if libraries.is_empty() {
        return Err("hya-cli-driver-example cdylib not built".into());
    }
    Ok(libraries)
}

/// Package the example bundle with `library`, stripped, as `runtime.library`.
fn pack_example(scratch: &Scratch, library: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let src = scratch.0.join("src");
    std::fs::create_dir_all(src.join("native"))?;
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundles/examples/cli-driver-example/bundle.yaml"),
    )?
    .replace("crate_dir: native", &format!("library: native/{LIB}"));
    std::fs::write(src.join("bundle.yaml"), manifest)?;
    let lib = src.join("native").join(LIB);
    let strip = Command::new("strip")
        .arg("--strip-debug")
        .arg("-o")
        .arg(&lib)
        .arg(library)
        .output()?;
    assert_success("strip example library", &strip);
    let pkg = scratch.0.join("example.hyabundle");
    let pack = scratch
        .hya()
        .arg("bundle")
        .arg("pack")
        .arg(&src)
        .arg(&pkg)
        .output()?;
    assert_success("bundle pack", &pack);
    Ok(pkg)
}

fn with_bare(scratch: &Scratch, bare: bool) -> Command {
    let mut command = scratch.hya();
    if bare {
        command.arg("--bare");
    }
    command.args(["--model", "hya/offline"]);
    command
}

/// A scratch home with the example installed from the cdylib that carries
/// this `hya`'s ABI digest.
fn installed_example(prefix: &str) -> Result<Scratch, Box<dyn std::error::Error>> {
    for library in example_libraries()? {
        let scratch = Scratch::new(prefix)?;
        let pkg = pack_example(&scratch, &library)?;
        let install = scratch
            .hya()
            .args(["bundle", "install", "-y"])
            .arg(&pkg)
            .output()?;
        assert_success("bundle install", &install);
        let probe = with_bare(&scratch, false)
            .args(["example-echo", "--text", "probe"])
            .output()?;
        if !String::from_utf8_lossy(&probe.stderr).contains("different hya-runtime-abi") {
            assert_success("example-echo probe", &probe);
            return Ok(scratch);
        }
    }
    Err("no hya-cli-driver-example cdylib matches this hya build; build both in one Cargo invocation (`cargo test -p hya-backend -p hya-cli-driver-example`, or `--workspace`)".into())
}

#[test]
fn installed_native_bundle_commands_route_to_its_driver_and_api() -> TestResult {
    let scratch = installed_example("installed-cli")?;
    for bare in [false, true] {
        let echo = with_bare(&scratch, bare)
            .args(["example-echo", "--text", "hi", "--times", "2", "--loud"])
            .output()?;
        assert_success("example-echo", &echo);
        assert_eq!(String::from_utf8(echo.stdout)?, "HI\nHI\n", "bare={bare}");

        let status = with_bare(&scratch, bare)
            .args(["example-status", "--name", "probe"])
            .output()?;
        assert_success("example-status", &status);
        assert_eq!(
            String::from_utf8(status.stdout)?,
            "probe: ok\n",
            "bare={bare}"
        );
    }
    Ok(())
}

const EXIT_DB_IN_USE: i32 = 75;

/// `hya example-session …` on `--db`: its stdout's session id.
fn session_on_db(scratch: &Scratch, extra: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let out = with_bare(scratch, false)
        .arg("--db")
        .arg(scratch.db())
        .arg("example-session")
        .args(extra)
        .output()?;
    assert_success("example-session", &out);
    let stdout = String::from_utf8(out.stdout)?;
    let mut lines = stdout.lines();
    let session = lines
        .next()
        .and_then(|line| line.strip_prefix("session "))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| format!("no session line: {stdout:?}"))?;
    assert_eq!(
        lines.next(),
        Some(format!("cwd {}", scratch.0.display()).as_str()),
        "{stdout}"
    );
    Ok(session.to_string())
}

async fn server_root_sessions(url: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let body: serde_json::Value = reqwest::get(format!("{url}/v2/sessions?includeArchived=true"))
        .await?
        .json()
        .await?;
    Ok(body["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["parent"].as_str().is_none_or(str::is_empty))
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect())
}

fn start_server(scratch: &Scratch) -> Result<ForegroundServer, Box<dyn std::error::Error>> {
    let mut serve = scratch.hya();
    serve
        .args([
            "--model",
            "hya/offline",
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--db",
        ])
        .arg(scratch.db());
    let discovery = PathBuf::from(format!("{}.server.json", scratch.db().display()));
    ForegroundServer::start(serve, "hya server listening on ", Some(&discovery))
}

/// `database: true` commands claim `--db` and run in process, with a new or
/// the named Session for a Session-scope endpoint and none for a Global one.
#[test]
fn database_commands_claim_the_db_and_run_in_process() -> TestResult {
    let scratch = installed_example("installed-cli-db")?;
    std::fs::create_dir_all(scratch.0.join("state"))?;
    let status = with_bare(&scratch, false)
        .arg("--db")
        .arg(scratch.db())
        .args(["example-db-status", "--name", "probe"])
        .output()?;
    assert_success("example-db-status", &status);
    assert_eq!(String::from_utf8(status.stdout)?, "probe: ok\n");
    assert!(
        scratch.db().is_file(),
        "the claimed database was not opened"
    );

    let created = session_on_db(&scratch, &[])?;
    assert_eq!(session_on_db(&scratch, &["--session", &created])?, created);
    let lock = scratch.0.join("state/sessions.db.runtime-owner.lock");
    assert!(std::fs::read_to_string(lock)?.trim().is_empty());
    Ok(())
}

/// In process, `routed_refuse` does not apply; a missing Session and an
/// invalid `--db` fail before the call.
#[test]
fn database_commands_in_process_ignore_refusals_and_reject_bad_targets() -> TestResult {
    let scratch = installed_example("installed-cli-db-errors")?;
    std::fs::create_dir_all(scratch.0.join("state"))?;
    session_on_db(&scratch, &["--slow"])?;

    let missing = hya_proto::SessionId::new().to_string();
    let out = with_bare(&scratch, false)
        .arg("--db")
        .arg(scratch.db())
        .args(["example-session", "--session", &missing])
        .output()?;
    assert_eq!(out.status.code(), Some(1), "{}", combined_text(&out));
    assert_eq!(
        String::from_utf8(out.stderr)?,
        format!("Error: example-session Session `{missing}` was not found\n")
    );

    let uri = with_bare(&scratch, false)
        .args(["--db", "file:x.db?mode=rwc", "example-db-status"])
        .output()?;
    assert!(!uri.status.success(), "{}", combined_text(&uri));
    assert!(!scratch.0.join("x.db").exists());
    Ok(())
}

/// With a server holding `--db`, `database: true` commands go through it:
/// a Global call creates no session, a Session call creates one or uses
/// the named one, and `routed_refuse` entries exit 75 before any session.
#[tokio::test]
async fn database_commands_route_through_the_owning_server() -> TestResult {
    let scratch = installed_example("installed-cli-routed")?;
    std::fs::create_dir_all(scratch.0.join("state"))?;
    let server = start_server(&scratch)?;

    let status = with_bare(&scratch, false)
        .arg("--db")
        .arg(scratch.db())
        .args(["example-db-status", "--name", "routed"])
        .output()?;
    assert_success("routed example-db-status", &status);
    assert_eq!(String::from_utf8(status.stdout)?, "routed: ok\n");
    assert!(server_root_sessions(&server.url).await?.is_empty());

    let created = session_on_db(&scratch, &[])?;
    assert_eq!(
        server_root_sessions(&server.url).await?,
        vec![created.clone()]
    );
    assert_eq!(session_on_db(&scratch, &["--session", &created])?, created);

    for (args, reason) in [
        (
            &["example-session", "--slow"][..],
            "`example-session --slow` cannot be sent to it",
        ),
        (
            &["--yolo", "example-session"][..],
            "--yolo cannot apply to it",
        ),
    ] {
        let out = with_bare(&scratch, false)
            .arg("--db")
            .arg(scratch.db())
            .args(args)
            .output()?;
        let stderr = String::from_utf8(out.stderr)?;
        assert_eq!(out.status.code(), Some(EXIT_DB_IN_USE), "{stderr}");
        assert!(
            stderr.starts_with("hya example-session: database ")
                && stderr.contains(&server.url)
                && stderr.contains(reason),
            "{stderr}"
        );
    }
    assert_eq!(server_root_sessions(&server.url).await?.len(), 1);
    Ok(())
}

/// A routed call has no client request timeout: it outlives the SDK's 30 s
/// deadline for ordinary calls.
#[test]
#[ignore = "takes 31 s, beyond the CI per-test limit; run with --ignored"]
fn routed_database_command_outlives_the_ordinary_request_timeout() -> TestResult {
    let scratch = installed_example("installed-cli-slow")?;
    std::fs::create_dir_all(scratch.0.join("state"))?;
    let _server = start_server(&scratch)?;
    session_on_db(&scratch, &["--delay-ms", "31000"])?;
    Ok(())
}
