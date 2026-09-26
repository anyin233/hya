//! `hya sessions`: archived root sessions are hidden unless `--all` or
//! `--archived`; `hya sessions archive|unarchive <id>` writes the database
//! directly, or goes through the live server that holds it (docs/cli.md
//! "`hya sessions`").

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_proto::{AgentName, Event, ModelRef, SessionId};
use hya_store::SessionStore;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("state"))?;
    Ok(dir)
}

fn hya(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .stdin(Stdio::null());
    command
}

fn sessions(root: &Path, db: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(hya(root)
        .arg("sessions")
        .args(args)
        .arg("--db")
        .arg(db)
        .output()?)
}

/// Session ids printed by `hya sessions` (first column).
fn listed(
    root: &Path,
    db: &Path,
    args: &[&str],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = sessions(root, db, args)?;
    assert!(output.status.success(), "{output:?}");
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| line.starts_with("hysec_"))
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect())
}

async fn seed(
    db: &Path,
    parent: Option<SessionId>,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let store = SessionStore::connect(&db.to_string_lossy()).await?;
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await?;
    Ok(session)
}

#[tokio::test]
async fn sessions_hides_archived_roots_and_archive_round_trips() -> TestResult {
    let root = scratch("hya-sessions-archive")?;
    let db = root.join("state/sessions.db");
    let kept = seed(&db, None).await?.to_string();
    let parked = seed(&db, None).await?;
    let child = seed(&db, Some(parked)).await?.to_string();
    let parked = parked.to_string();

    let output = sessions(&root, &db, &["archive", &parked])?;
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8(output.stdout)?.contains(&format!("archived {parked}")));

    let default = listed(&root, &db, &[])?;
    assert!(!default.contains(&parked), "{default:?}");
    assert!(default.contains(&kept) && default.contains(&child));
    let all = listed(&root, &db, &["--all"])?;
    assert!(all.contains(&parked) && all.contains(&kept) && all.contains(&child));
    assert_eq!(
        listed(&root, &db, &["--archived"])?,
        std::slice::from_ref(&parked)
    );

    // A child cannot be archived; an unknown id fails.
    let output = sessions(&root, &db, &["archive", &child])?;
    assert!(!output.status.success(), "{output:?}");
    let missing = SessionId::new().to_string();
    let output = sessions(&root, &db, &["archive", &missing])?;
    assert!(!output.status.success(), "{output:?}");

    let output = sessions(&root, &db, &["unarchive", &parked])?;
    assert!(output.status.success(), "{output:?}");
    assert!(listed(&root, &db, &[])?.contains(&parked));
    assert!(listed(&root, &db, &["--archived"])?.is_empty());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A running `hya serve`: killed on drop.
struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(root: &Path, db: &Path) -> Result<Server, Box<dyn std::error::Error>> {
    let mut child = hya(root)
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(db)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                let _ = tx.send(url.trim().to_string());
            }
        }
    });
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    }
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(url) => Ok(Server { child, url }),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("hya serve printed no readiness line".into())
        }
    }
}

#[tokio::test]
async fn archive_goes_through_the_server_that_holds_the_database() -> TestResult {
    let root = scratch("hya-sessions-archive-live")?;
    let db = root.join("state/sessions.db");
    let session = seed(&db, None).await?.to_string();
    let server = serve(&root, &db)?;
    // Wait for the discovery file the CLI attaches through.
    let discovery = PathBuf::from(format!("{}.server.json", db.display()));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !discovery.is_file() {
        assert!(std::time::Instant::now() < deadline, "no discovery file");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let output = sessions(&root, &db, &["archive", &session])?;
    assert!(output.status.success(), "{output:?}");
    let client = reqwest::Client::new();
    let info: serde_json::Value = client
        .get(format!("{}/v1/sessions/{session}", server.url))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(info["archived"], serde_json::json!(true), "{info}");

    let output = sessions(&root, &db, &["unarchive", &session])?;
    assert!(output.status.success(), "{output:?}");
    let info: serde_json::Value = client
        .get(format!("{}/v1/sessions/{session}", server.url))
        .send()
        .await?
        .json()
        .await?;
    assert!(info.get("archived").is_none(), "{info}");
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
