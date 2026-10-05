//! Bundle management over the v1 API against a real daemon (`ListBundles`,
//! `InstallBundle`, `UninstallBundle`, `SetBundleEnabled`): what each call
//! changes in the listing and in the published catalog (`ListTuiExtensions`).

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const BUNDLE: &str = "e2e/tui-panel";

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.canonicalize()?)
}

/// `hya serve start --json` with an isolated HOME/XDG under `root`; its pid and URL.
fn start(root: &Path) -> Result<(i32, String), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_hya"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .args(["serve", "start", "--json", "--db"])
        .arg(root.join("s.db"))
        .stdin(Stdio::null())
        .output()?;
    let info: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "not JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    let pid = i32::try_from(info["pid"].as_i64().ok_or("no pid")?)?;
    Ok((pid, info["url"].as_str().ok_or("no url")?.to_string()))
}

/// Kills the daemon even when the test fails half-way.
struct Daemon(i32);

impl Drop for Daemon {
    fn drop(&mut self) {
        // SAFETY: `kill` has no memory-safety preconditions.
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
    }
}

/// One JSON request; the status and the parsed body.
fn call(
    url: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Result<(u16, Value), Box<dyn std::error::Error>> {
    let authority = url
        .strip_prefix("http://")
        .ok_or("not http")?
        .trim_end_matches('/');
    let mut socket = TcpStream::connect(authority)?;
    socket.set_read_timeout(Some(Duration::from_secs(60)))?;
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    write!(
        socket,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{payload}",
        payload.len()
    )?;
    socket.flush()?;
    let mut raw = String::new();
    socket.read_to_string(&mut raw)?;
    let (head, text) = raw.split_once("\r\n\r\n").ok_or("no HTTP body")?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("no HTTP status")?;
    let text = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(text)
    } else {
        text.to_string()
    };
    let parsed =
        serde_json::from_str(text.trim()).map_err(|error| format!("not JSON ({error}): {text}"))?;
    Ok((status, parsed))
}

fn dechunk(body: &str) -> String {
    let mut text = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || after.len() < size {
            break;
        }
        text.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    text
}

/// A Plugin bundle with a TUI extension, packaged at `dir/tui-panel.hyabundle`.
fn package(dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest = "kind: Plugin\nidentity: { id: e2e/tui-panel, version: 1.0.0, publisher: e2e }\nextensions:\n  files:\n    - { id: panel, path: tui/main.ts }\ntui:\n  api_version: 1\n  entry: tui/main.ts\n  sdk: 1.0.0\n  permissions: [tui.panel]\n";
    let source = hya_bundle::BundleSource::new(
        "tui-panel",
        vec![
            hya_bundle::SourceFile::new("bundle.yaml", manifest.as_bytes().to_vec()),
            hya_bundle::SourceFile::new("tui/main.ts", b"export default {}\n".to_vec()),
        ],
    );
    let path = dir.join("tui-panel.hyabundle");
    std::fs::write(&path, hya_bundle::write_public_package(&source)?)?;
    Ok(path)
}

/// The `ListBundles` row of `id` in `scope`, if listed.
fn row(
    url: &str,
    directory: &str,
    id: &str,
    scope: &str,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let path = format!("/v1/bundles?directory={}", encode(directory));
    let (status, body) = call(url, "GET", &path, &Value::Null)?;
    assert_eq!(status, 200, "{body}");
    Ok(body["bundles"]
        .as_array()
        .ok_or("no bundles")?
        .iter()
        .find(|row| row["id"] == id && row["scope"] == scope)
        .cloned())
}

/// Bundle ids with a TUI extension in the directory's published catalog.
fn tui_extensions(url: &str, directory: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let path = format!("/v1/tui-extensions?directory={}", encode(directory));
    let (status, body) = call(url, "GET", &path, &Value::Null)?;
    assert_eq!(status, 200, "{body}");
    Ok(body["extensions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["bundleId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[test]
fn bundles_install_disable_enable_and_uninstall_through_the_api() -> TestResult {
    let root = scratch("hya-bundle-api")?;
    let (pid, url) = start(&root)?;
    let _daemon = Daemon(pid);
    let package = package(&root)?;

    // First-party bundles are listed but cannot be removed.
    let basic = row(&url, "", "hya/basic-tui-components", "first_party")?
        .ok_or("first-party bundle not listed")?;
    // proto3 JSON omits `false`.
    assert_ne!(basic["removable"], true);
    assert_eq!(basic["components"]["tui"], true);
    assert!(row(&url, "", BUNDLE, "user")?.is_none());

    // A relative path is refused before anything is read.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:install",
        &json!({ "path": "tui-panel.hyabundle" }),
    )?;
    assert_eq!(status, 400, "{body}");

    // Install: listed with its TUI extension, and published.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:install",
        &json!({ "path": package.to_string_lossy() }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["bundleId"], BUNDLE);
    let installed = row(&url, "", BUNDLE, "user")?.ok_or("installed bundle not listed")?;
    assert_eq!(installed["state"], "active");
    assert_eq!(installed["removable"], true);
    assert_eq!(
        installed["components"]["tuiPermissions"],
        json!(["tui.panel"])
    );
    assert!(tui_extensions(&url, "")?.contains(&BUNDLE.to_string()));

    // Disable: still listed as disabled, no longer published.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:set-enabled",
        &json!({ "bundleId": BUNDLE, "enabled": false }),
    )?;
    assert_eq!(status, 200, "{body}");
    let disabled = row(&url, "", BUNDLE, "user")?.ok_or("disabled bundle not listed")?;
    assert_eq!(disabled["state"], "disabled");
    assert_ne!(disabled["enabled"], true, "proto3 JSON omits false");
    assert!(!tui_extensions(&url, "")?.contains(&BUNDLE.to_string()));

    // Enable again: published again.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:set-enabled",
        &json!({ "bundleId": BUNDLE, "enabled": true }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(tui_extensions(&url, "")?.contains(&BUNDLE.to_string()));

    // A disabled first-party bundle stops publishing its TUI extension too; trusted presets cannot be disabled.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:set-enabled",
        &json!({ "bundleId": "hya/basic-tui-components", "enabled": false }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(!tui_extensions(&url, "")?.contains(&"hya/basic-tui-components".to_string()));
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:set-enabled",
        &json!({ "bundleId": "hya/basic-tui-components", "enabled": true }),
    )?;
    assert_eq!(status, 200, "{body}");
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:set-enabled",
        &json!({ "bundleId": "hya/core-agents", "enabled": false }),
    )?;
    assert_eq!(status, 409, "{body}");

    // First-party bundles cannot be uninstalled; installed ones can.
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:uninstall",
        &json!({ "bundleId": "hya/goal-loop" }),
    )?;
    assert_eq!(status, 409, "{body}");
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:uninstall",
        &json!({ "bundleId": BUNDLE }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(row(&url, "", BUNDLE, "user")?.is_none());
    assert!(!tui_extensions(&url, "")?.contains(&BUNDLE.to_string()));
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:uninstall",
        &json!({ "bundleId": BUNDLE }),
    )?;
    assert_eq!(status, 404, "{body}");

    // Project scope: `<directory>/.hya/bundles`.
    let project = root.join("work");
    std::fs::create_dir_all(&project)?;
    let directory = project.to_string_lossy().into_owned();
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:install",
        &json!({ "directory": directory, "path": package.to_string_lossy(), "project": true }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(project.join(".hya/bundles").read_dir()?.next().is_some());
    assert_eq!(
        row(&url, &directory, BUNDLE, "project")?.ok_or("project bundle not listed")?["state"],
        "active"
    );
    assert!(
        row(&url, "", BUNDLE, "project")?.is_none(),
        "no directory: no project bundles"
    );
    let (status, body) = call(
        &url,
        "POST",
        "/v1/bundles:uninstall",
        &json!({ "directory": directory, "bundleId": BUNDLE, "project": true }),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(row(&url, &directory, BUNDLE, "project")?.is_none());
    Ok(())
}
