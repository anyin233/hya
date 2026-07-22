//! Load Grok Build OAuth session credentials from `~/.grok/auth.json`.
//!
//! Grok Build (`grok login`) stores an OIDC access JWT under field `key`, plus an
//! optional `refresh_token` and `expires_at`. The hya `grok-build` provider prefers
//! this session credential over a static API key so requests can use the CLI chat
//! proxy the same way Grok Build does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// One Grok Build session credential resolved from `auth.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrokOauthCredential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
    pub create_time: Option<OffsetDateTime>,
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub source_path: PathBuf,
}

impl GrokOauthCredential {
    /// Returns true when `expires_at` is absent or still after `now + skew`.
    #[must_use]
    pub fn is_fresh(&self, skew: Duration) -> bool {
        let Some(expires_at) = self.expires_at else {
            return true;
        };
        let Some(deadline) = OffsetDateTime::now_utc().checked_add(skew_to_time(skew)) else {
            return false;
        };
        expires_at > deadline
    }
}

/// Resolve `$GROK_HOME/auth.json`, else `$HOME/.grok/auth.json`.
#[must_use]
pub fn auth_json_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        return Some(PathBuf::from(home).join("auth.json"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".grok/auth.json"))
}

/// Load the best available Grok Build OAuth credential from the default path.
#[must_use]
pub fn load_grok_oauth() -> Option<GrokOauthCredential> {
    load_grok_oauth_from(auth_json_path()?)
}

/// Load the best available credential from an explicit `auth.json` path.
#[must_use]
pub fn load_grok_oauth_from(path: impl AsRef<Path>) -> Option<GrokOauthCredential> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).ok()?;
    parse_grok_auth_json(&raw, path)
}

/// Parse a Grok Build `auth.json` document and pick the newest usable entry.
#[must_use]
pub fn parse_grok_auth_json(raw: &str, source_path: impl AsRef<Path>) -> Option<GrokOauthCredential> {
    let map: BTreeMapJson = serde_json::from_str(raw).ok()?;
    let mut best: Option<GrokOauthCredential> = None;
    for (_key, entry) in map.0 {
        let Some(access_token) = entry
            .key
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        let candidate = GrokOauthCredential {
            access_token,
            refresh_token: non_empty(entry.refresh_token),
            expires_at: parse_rfc3339(entry.expires_at.as_deref()),
            create_time: parse_rfc3339(entry.create_time.as_deref()),
            oidc_issuer: non_empty(entry.oidc_issuer),
            oidc_client_id: non_empty(entry.oidc_client_id),
            source_path: source_path.as_ref().to_path_buf(),
        };
        best = Some(match best {
            None => candidate,
            Some(current) => prefer_newer(current, candidate),
        });
    }
    best
}

fn prefer_newer(a: GrokOauthCredential, b: GrokOauthCredential) -> GrokOauthCredential {
    match (a.create_time, b.create_time) {
        (Some(ta), Some(tb)) if tb > ta => b,
        (None, Some(_)) => b,
        _ => a,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn parse_rfc3339(value: Option<&str>) -> Option<OffsetDateTime> {
    let value = value?.trim();
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn skew_to_time(skew: Duration) -> time::Duration {
    // time::Duration::seconds saturates on conversion failure.
    time::Duration::seconds(i64::try_from(skew.as_secs()).unwrap_or(i64::MAX))
}

#[derive(Debug, Deserialize)]
struct BTreeMapJson(std::collections::BTreeMap<String, AuthEntry>);

#[derive(Debug, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    create_time: Option<String>,
    #[serde(default)]
    oidc_issuer: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-grok-oauth-{nanos}-{serial}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SAMPLE: &str = r#"{
  "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
    "key": "eyJhbGciOiJFUzI1NiJ9.sample-access-token",
    "auth_mode": "oidc",
    "create_time": "2026-07-22T08:27:38.968401159Z",
    "refresh_token": "refresh-sample",
    "expires_at": "2026-07-22T14:27:38.968401159Z",
    "oidc_issuer": "https://auth.x.ai",
    "oidc_client_id": "b1a00492-073a-47ea-816f-4c329264a828"
  }
}"#;

    #[test]
    fn parse_single_entry_returns_access_token() {
        let path = PathBuf::from("/tmp/fake-auth.json");
        let cred = parse_grok_auth_json(SAMPLE, &path).expect("parsed");
        assert_eq!(cred.access_token, "eyJhbGciOiJFUzI1NiJ9.sample-access-token");
        assert_eq!(cred.refresh_token.as_deref(), Some("refresh-sample"));
        assert_eq!(cred.oidc_issuer.as_deref(), Some("https://auth.x.ai"));
        assert_eq!(
            cred.oidc_client_id.as_deref(),
            Some("b1a00492-073a-47ea-816f-4c329264a828")
        );
        assert_eq!(cred.source_path, path);
        assert!(cred.expires_at.is_some());
        assert!(cred.create_time.is_some());
    }

    #[test]
    fn missing_and_empty_documents_yield_none() {
        let path = PathBuf::from("/tmp/missing-auth.json");
        assert!(parse_grok_auth_json("", &path).is_none());
        assert!(parse_grok_auth_json("{}", &path).is_none());
        assert!(parse_grok_auth_json(r#"{"x":{"key":""}}"#, &path).is_none());
        assert!(load_grok_oauth_from(path).is_none());
    }

    #[test]
    fn prefers_newest_create_time_among_entries() {
        let raw = r#"{
  "old": {
    "key": "old-token",
    "create_time": "2026-01-01T00:00:00Z"
  },
  "new": {
    "key": "new-token",
    "create_time": "2026-07-22T00:00:00Z"
  }
}"#;
        let cred = parse_grok_auth_json(raw, PathBuf::from("auth.json")).unwrap();
        assert_eq!(cred.access_token, "new-token");
    }

    #[test]
    fn is_fresh_false_when_expired() {
        let mut cred = parse_grok_auth_json(SAMPLE, PathBuf::from("auth.json")).unwrap();
        // Force an expired timestamp.
        cred.expires_at = Some(OffsetDateTime::UNIX_EPOCH);
        assert!(!cred.is_fresh(Duration::from_secs(0)));
        cred.expires_at = None;
        assert!(cred.is_fresh(Duration::from_secs(300)));
    }

    #[test]
    fn load_from_disk_reads_auth_json() {
        let dir = tempdir();
        let path = dir.join("auth.json");
        std::fs::write(&path, SAMPLE).unwrap();
        let cred = load_grok_oauth_from(&path).expect("loaded");
        assert_eq!(cred.access_token, "eyJhbGciOiJFUzI1NiJ9.sample-access-token");
        assert_eq!(cred.source_path, path);
        let _ = std::fs::remove_dir_all(dir);
    }
}
