//! Integration tests for `hya provider add|list|remove` against a local fake
//! model-list endpoint.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unique_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "hya-provider-cli-{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root)?;
    Ok(root)
}

/// A model-list endpoint that answers `GET …/models` with its current model
/// ids (`alpha` and `beta` at first) when the request carries `key` (Bearer
/// or `x-api-key`), else 401. Every request's head is recorded.
struct FakeModels {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    models: Arc<Mutex<Vec<&'static str>>>,
}

impl FakeModels {
    fn start(key: &'static str) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let models = Arc::new(Mutex::new(vec!["alpha", "beta"]));
        let seen = Arc::clone(&requests);
        let listed = Arc::clone(&models);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(clone) => clone,
                    Err(_) => continue,
                });
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let lower = head.to_ascii_lowercase();
                let authorized = lower.contains(&format!("authorization: bearer {key}"))
                    || lower.contains(&format!("x-api-key: {key}"));
                if let Ok(mut seen) = seen.lock() {
                    seen.push(head);
                }
                let data = listed
                    .lock()
                    .map(|ids| {
                        ids.iter()
                            .map(|id| format!(r#"{{"id":"{id}","type":"model"}}"#))
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let (status, body) = if authorized {
                    ("200 OK", format!(r#"{{"data":[{data}],"has_more":false}}"#))
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"error":{"message":"bad key"}}"#.to_string(),
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let mut rest = Vec::new();
                let _ = reader.get_mut().set_nonblocking(true);
                let _ = reader.read_to_end(&mut rest);
            }
        });
        Ok(Self {
            base,
            requests,
            models,
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// How many model lists were fetched so far.
    fn fetches(&self) -> usize {
        self.requests()
            .iter()
            .filter(|head| head.starts_with("GET /v1/models"))
            .count()
    }

    fn set_models(&self, ids: &[&'static str]) {
        if let Ok(mut models) = self.models.lock() {
            *models = ids.to_vec();
        }
    }
}

/// `GET /v1/models?providerId=fake` of the backend at `url` (the list the
/// TUI and WebUI read); returns the response body.
fn backend_models(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let host = url.trim_start_matches("http://").trim_end_matches('/');
    let mut stream = std::net::TcpStream::connect(host)?;
    write!(
        stream,
        "GET /v1/models?providerId=fake HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn hya(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("HOME", root)
        .env_remove("HYA_DB")
        .current_dir(root);
    command
}

/// Run `hya provider <args>` with `input` on stdin (closed afterwards).
fn provider(root: &Path, args: &[&str], input: &str) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = hya(root)
        .arg("provider")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("child stdin missing")?;
    stdin.write_all(input.as_bytes())?;
    drop(stdin);
    Ok(child.wait_with_output()?)
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn config(root: &Path) -> String {
    fs::read_to_string(root.join("config/hya/config.yaml")).unwrap_or_default()
}

#[test]
fn add_walks_through_the_prompts_then_list_and_remove_manage_the_provider() -> TestResult {
    let root = unique_root()?;
    let server = FakeModels::start("sk-test")?;

    // Base URL, protocol (3 = anthropic-messages), API key, provider name.
    let add = provider(
        &root,
        &["add"],
        &format!("{}\n3\nsk-test\nfake\n", server.base),
    )?;
    let out = text(&add);
    assert!(add.status.success(), "{out}");
    assert!(out.contains("alpha") && out.contains("beta"), "{out}");
    assert!(out.contains("Saved provider `fake`"), "{out}");
    // Anthropic Messages lists models with the key in `x-api-key`.
    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|head| head.starts_with("GET /v1/models")
                && head.to_ascii_lowercase().contains("x-api-key: sk-test")),
        "{requests:?}"
    );

    let yaml = config(&root);
    assert!(
        yaml.contains("fake:") && yaml.contains("kind: anthropic"),
        "{yaml}"
    );
    assert!(
        yaml.contains(&format!("base_url: {}", server.base)),
        "{yaml}"
    );
    assert!(
        !yaml.contains("sk-test"),
        "the key must not be written to config.yaml:\n{yaml}"
    );
    let key_file = root.join("config/hya/auth/fake.yaml");
    assert!(fs::read_to_string(&key_file)?.contains("sk-test"));

    let list = provider(&root, &["list"], "")?;
    let listed = text(&list);
    assert!(list.status.success(), "{listed}");
    for needle in [
        "fake",
        "anthropic-messages",
        server.base.as_str(),
        "saved key",
        "fake/alpha",
        "fake/beta",
    ] {
        assert!(listed.contains(needle), "list lacks `{needle}`:\n{listed}");
    }

    // Declining keeps everything; confirming removes the entry and its key.
    let kept = provider(&root, &["remove", "fake"], "n\n")?;
    assert!(!kept.status.success(), "{}", text(&kept));
    assert!(config(&root).contains("fake:"));
    let removed = provider(&root, &["remove", "fake"], "y\n")?;
    assert!(removed.status.success(), "{}", text(&removed));
    assert!(!config(&root).contains("fake:"), "{}", config(&root));
    assert!(!key_file.exists());
    let after = text(&provider(&root, &["list"], "")?);
    assert!(!after.contains("fake/alpha"), "{after}");

    let missing = provider(&root, &["remove", "fake", "--yes"], "")?;
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("not configured"),
        "{}",
        text(&missing)
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn add_writes_nothing_when_the_endpoint_rejects_the_key() -> TestResult {
    let root = unique_root()?;
    let server = FakeModels::start("sk-good")?;
    let args = [
        "add",
        "--name",
        "bad",
        "--base-url",
        server.base.as_str(),
        "--protocol",
        "openai-chat",
        "--api-key",
        "sk-wrong",
    ];
    // stdin closes before "Save anyway?" is answered: that is a no.
    let add = provider(&root, &args, "")?;
    let out = text(&add);
    assert!(!add.status.success(), "{out}");
    assert!(out.contains("rejected"), "{out}");
    // OpenAI Chat lists models with a Bearer token.
    assert!(
        server.requests().iter().any(|head| head
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-wrong")),
        "{:?}",
        server.requests()
    );
    assert!(!config(&root).contains("bad:"), "{}", config(&root));
    assert!(!root.join("config/hya/auth/bad.yaml").exists());

    // Flags skip every prompt; a correct key saves without asking.
    let good = provider(
        &root,
        &[
            "add",
            "--name",
            "good",
            "--base-url",
            server.base.as_str(),
            "--protocol",
            "openai-chat",
            "--api-key",
            "sk-good",
        ],
        "",
    )?;
    assert!(good.status.success(), "{}", text(&good));
    assert!(config(&root).contains("kind: openai"), "{}", config(&root));

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn list_marks_config_overrides_and_logout_drops_only_the_key() -> TestResult {
    let root = unique_root()?;
    let server = FakeModels::start("sk-test")?;
    let base = server.base.as_str();
    let add = provider(
        &root,
        &[
            "add",
            "--name",
            "fake",
            "--base-url",
            base,
            "--protocol",
            "openai-chat",
            "--api-key",
            "sk-test",
        ],
        "",
    )?;
    assert!(add.status.success(), "{}", text(&add));

    // A `models:` entry for a fetched model overrides it; one the endpoint
    // does not list is config-only.
    let path = root.join("config/hya/config.yaml");
    let yaml = fs::read_to_string(&path)?.replace(
        "    models: []",
        "    models:\n      - id: alpha\n        name: Alpha Custom\n      - gamma",
    );
    fs::write(&path, yaml)?;
    // A key saved for a provider that config.yaml does not declare.
    let login = hya(&root).args(["login", "orphan", "sk-orphan"]).output()?;
    assert!(login.status.success(), "{}", text(&login));

    let listed = text(&provider(&root, &["list"], "")?);
    let line = |needle: &str| {
        listed
            .lines()
            .find(|line| line.trim_start().starts_with(needle))
            .unwrap_or_default()
            .to_string()
    };
    assert!(line("fake/alpha").contains("config override"), "{listed}");
    assert!(line("fake/gamma").contains("config only"), "{listed}");
    let beta = line("fake/beta");
    assert!(!beta.is_empty() && !beta.contains("config"), "{listed}");
    assert!(
        listed.contains("orphan"),
        "saved keys without a provider are listed:\n{listed}"
    );

    // `providers` is the same command.
    let alias = hya(&root).args(["providers", "list"]).output()?;
    assert_eq!(text(&alias), listed);

    let logout = provider(&root, &["logout", "fake"], "")?;
    assert!(logout.status.success(), "{}", text(&logout));
    assert!(!root.join("config/hya/auth/fake.yaml").exists());
    assert!(
        config(&root).contains("fake:"),
        "logout keeps the provider:\n{}",
        config(&root)
    );
    let after = text(&provider(&root, &["list"], "")?);
    assert!(
        after
            .lines()
            .any(|line| line.starts_with("fake ") && line.ends_with("no key")),
        "{after}"
    );

    fs::remove_dir_all(root)?;
    Ok(())
}

/// With a backend running for the database, `--refresh` fetches through it:
/// the backend (and so the TUI and WebUI on it) lists the new models at once,
/// and the endpoint is asked once per provider, not by both processes.
#[test]
fn refresh_updates_the_running_backend() -> TestResult {
    let root = unique_root()?;
    let server = FakeModels::start("sk-test")?;
    let add = provider(
        &root,
        &[
            "add",
            "--name",
            "fake",
            "--base-url",
            &server.base,
            "--protocol",
            "openai-chat",
            "--api-key",
            "sk-test",
        ],
        "",
    )?;
    assert!(add.status.success(), "{}", text(&add));
    let db = root.join("s.db");
    let db = db.to_str().ok_or("db path")?;
    let started = hya(&root)
        .args(["serve", "start", "--json", "--db", db])
        .output()?;
    assert!(started.status.success(), "{}", text(&started));
    let ready: serde_json::Value = serde_json::from_slice(&started.stdout)?;
    let url = ready["url"].as_str().ok_or("serve start url")?.to_string();
    let result = (|| -> TestResult {
        let before = backend_models(&url)?;
        assert!(
            before.contains("fake/beta") && !before.contains("fake/gamma"),
            "{before}"
        );

        server.set_models(&["alpha", "beta", "gamma"]);
        let fetched = server.fetches();
        let listed = hya(&root)
            .args(["provider", "list", "--refresh", "--db", db])
            .output()?;
        assert!(listed.status.success(), "{}", text(&listed));
        assert!(text(&listed).contains("fake/gamma"), "{}", text(&listed));
        assert_eq!(server.fetches(), fetched + 1, "one fetch per refresh");
        let after = backend_models(&url)?;
        assert!(after.contains("fake/gamma"), "{after}");

        server.set_models(&["alpha", "beta", "gamma", "delta"]);
        let fetched = server.fetches();
        let models = hya(&root)
            .args(["models", "fake", "--refresh", "--db", db])
            .output()?;
        assert!(models.status.success(), "{}", text(&models));
        assert!(text(&models).contains("delta"), "{}", text(&models));
        assert_eq!(server.fetches(), fetched + 1, "one fetch per refresh");
        let after = backend_models(&url)?;
        assert!(after.contains("fake/delta"), "{after}");
        Ok(())
    })();
    let _ = hya(&root).args(["serve", "stop", "--db", db]).output();
    result?;
    fs::remove_dir_all(root)?;
    Ok(())
}
