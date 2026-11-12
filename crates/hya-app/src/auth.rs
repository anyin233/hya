//! Provider credential storage under `~/.config/hya/auth/<provider>.yaml`.
//!
//! Supports plain API tokens (`token: ...`) and OAuth bundles
//! (`type: oauth` with access/refresh/expiry fields).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Supported OAuth provider implementations for interactive login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OAuthType {
    /// ChatGPT Codex subscription OAuth (`openai-codex`).
    OpenaiCodex,
    /// Grok Build / xAI CLI OAuth (`grok-build`).
    GrokBuild,
}

impl OAuthType {
    /// Parse one canonical type id.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "openai-codex" => Some(Self::OpenaiCodex),
            "grok-build" => Some(Self::GrokBuild),
            _ => None,
        }
    }

    /// Canonical type id used in CLI flags and stored auth files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiCodex => "openai-codex",
            Self::GrokBuild => "grok-build",
        }
    }

    /// Config `kind` written when upserting a provider after OAuth login.
    #[must_use]
    pub const fn provider_kind(self) -> &'static str {
        match self {
            Self::OpenaiCodex => "openai-codex",
            Self::GrokBuild => "grok-build",
        }
    }

    /// Default inference base URL for this OAuth type.
    #[must_use]
    pub const fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenaiCodex => "https://chatgpt.com/backend-api/codex",
            Self::GrokBuild => "https://cli-chat-proxy.grok.com/v1",
        }
    }

    /// Default model id when none is supplied at login.
    #[must_use]
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::OpenaiCodex => "gpt-5.3-codex",
            Self::GrokBuild => "grok-4.5",
        }
    }
}

impl std::fmt::Display for OAuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Full OAuth credential bundle stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredential {
    /// Which OAuth implementation produced this bundle.
    pub oauth_type: OAuthType,
    /// Short-lived bearer access token for API calls.
    pub access_token: String,
    /// Long-lived refresh token used to mint new access tokens.
    pub refresh_token: String,
    /// RFC3339 UTC timestamp when the access token expires.
    pub expires_at: String,
    /// Optional ChatGPT/Grok account id from token claims when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Optional OpenID id_token (not used as the API bearer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
}

/// On-disk auth document for one provider id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthCredential {
    /// Static API key / pasted bearer token.
    Api {
        /// Raw token string written under `token:` in the auth YAML.
        token: String,
    },
    /// OAuth subscription credential with refresh support.
    OAuth(OAuthCredential),
}

impl AuthCredential {
    /// Bearer material currently usable for HTTP Authorization.
    #[must_use]
    pub fn access_token(&self) -> &str {
        match self {
            Self::Api { token } => token,
            Self::OAuth(oauth) => oauth.access_token.as_str(),
        }
    }

    /// OAuth metadata when this credential is OAuth-backed.
    #[must_use]
    pub fn oauth(&self) -> Option<&OAuthCredential> {
        match self {
            Self::OAuth(oauth) => Some(oauth),
            Self::Api { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct AuthFile {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    oauth_type: Option<OAuthType>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

impl AuthFile {
    /// Build the credential this document describes.
    ///
    /// A document that is valid YAML but does not carry a complete credential
    /// is an error, not an absent credential: it means the file is damaged.
    fn into_credential(self) -> std::io::Result<AuthCredential> {
        let kind = self
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("api");
        if kind.eq_ignore_ascii_case("oauth") {
            let oauth_type = self
                .oauth_type
                .ok_or_else(|| invalid_credential("missing oauth_type"))?;
            return Ok(AuthCredential::OAuth(OAuthCredential {
                oauth_type,
                access_token: required_field(self.access_token, "access_token")?,
                refresh_token: required_field(self.refresh_token, "refresh_token")?,
                expires_at: required_field(self.expires_at, "expires_at")?,
                account_id: optional_field(self.account_id),
                id_token: optional_field(self.id_token),
            }));
        }
        let token = optional_field(self.token)
            .or_else(|| optional_field(self.access_token))
            .ok_or_else(|| invalid_credential("missing token"))?;
        Ok(AuthCredential::Api { token })
    }

    fn from_credential(cred: &AuthCredential) -> Self {
        match cred {
            AuthCredential::Api { token } => Self {
                kind: Some("api".to_string()),
                token: Some(token.clone()),
                oauth_type: None,
                access_token: None,
                refresh_token: None,
                expires_at: None,
                account_id: None,
                id_token: None,
            },
            AuthCredential::OAuth(oauth) => Self {
                kind: Some("oauth".to_string()),
                token: None,
                oauth_type: Some(oauth.oauth_type),
                access_token: Some(oauth.access_token.clone()),
                refresh_token: Some(oauth.refresh_token.clone()),
                expires_at: Some(oauth.expires_at.clone()),
                account_id: oauth.account_id.clone(),
                id_token: oauth.id_token.clone(),
            },
        }
    }
}

/// Resolve the hya auth directory (`$XDG_CONFIG_HOME/hya/auth` or `~/.config/hya/auth`).
pub fn auth_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("hya/auth"))
}

/// Save a plain API token for `provider` under `dir`.
pub fn save_token_in(dir: &Path, provider: &str, token: &str) -> std::io::Result<()> {
    save_credential_in(
        dir,
        provider,
        &AuthCredential::Api {
            token: token.trim().to_string(),
        },
    )
}

/// Save a full credential document for `provider` under `dir`.
pub fn save_credential_in(
    dir: &Path,
    provider: &str,
    credential: &AuthCredential,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file = AuthFile::from_credential(credential);
    let body = serde_norway::to_string(&file).map_err(std::io::Error::other)?;
    let path = dir.join(format!("{provider}.yaml"));
    let tmp = dir.join(format!(".{provider}.yaml.tmp"));
    // Create the temp file with mode 0600 so the secret is never readable
    // by others, even between the write and the rename.
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// List provider ids that have a credential file under `dir`.
pub fn list_tokens_in(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut providers = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
                    continue;
                }
                if let Some(provider) = path.file_stem().and_then(|stem| stem.to_str()) {
                    if provider.starts_with('.') {
                        continue;
                    }
                    providers.push(provider.to_string());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    providers.sort();
    Ok(providers)
}

/// Remove the credential file for `provider` under `dir`.
pub fn remove_token_in(dir: &Path, provider: &str) -> std::io::Result<bool> {
    match std::fs::remove_file(dir.join(format!("{provider}.yaml"))) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Load the bearer access token for `provider` under `dir`, if present.
///
/// # Errors
/// Same as [`load_credential_in`].
pub fn load_token_in(dir: &Path, provider: &str) -> std::io::Result<Option<String>> {
    Ok(load_credential_in(dir, provider)?.map(|credential| credential.access_token().to_string()))
}

/// Load the full credential document for `provider` under `dir`.
///
/// `Ok(None)` means no credential file exists. A file that cannot be read or
/// parsed, and one that lacks a complete credential, are errors.
///
/// # Errors
/// Returns I/O or YAML failures, or an incomplete credential document.
pub fn load_credential_in(dir: &Path, provider: &str) -> std::io::Result<Option<AuthCredential>> {
    let path = dir.join(format!("{provider}.yaml"));
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!("read {}: {error}", path.display()),
            ));
        }
    };
    let file: AuthFile = serde_norway::from_str(&content)
        .map_err(|error| invalid_credential(&format!("parse {}: {error}", path.display())))?;
    let credential = file.into_credential().map_err(|error| {
        std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    })?;
    Ok(Some(credential))
}

fn invalid_credential(reason: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, reason.to_string())
}

fn required_field(value: Option<String>, name: &str) -> std::io::Result<String> {
    optional_field(value).ok_or_else(|| invalid_credential(&format!("missing {name}")))
}

fn optional_field(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Save a plain API token for `provider` in the user auth directory.
pub fn save_token(provider: &str, token: &str) -> std::io::Result<()> {
    let dir = auth_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    save_token_in(&dir, provider, token)
}

/// List provider ids with saved credentials in the user auth directory.
pub fn list_tokens() -> std::io::Result<Vec<String>> {
    let dir = auth_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    list_tokens_in(&dir)
}

/// Remove the credential for `provider` from the user auth directory.
pub fn remove_token(provider: &str) -> std::io::Result<bool> {
    let dir = auth_dir().ok_or_else(|| std::io::Error::other("no config directory"))?;
    remove_token_in(&dir, provider)
}

/// Load the full credential for `provider` from the user auth directory.
///
/// # Errors
/// Same as [`load_credential_in`].
pub fn load_credential(provider: &str) -> std::io::Result<Option<AuthCredential>> {
    match auth_dir() {
        Some(dir) => load_credential_in(&dir, provider),
        None => Ok(None),
    }
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
        let dir =
            std::env::temp_dir().join(format!("hya-auth-{nanos}-{serial}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_token_and_missing_is_none() {
        let dir = tempdir();
        assert_eq!(load_token_in(&dir, "anthropic").unwrap(), None);
        save_token_in(&dir, "anthropic", "  tok-123\n").unwrap();
        assert_eq!(
            load_token_in(&dir, "anthropic").unwrap(),
            Some("tok-123".to_string())
        );
        assert_eq!(load_token_in(&dir, "other").unwrap(), None);
    }

    #[test]
    fn token_is_stored_as_yaml() {
        let dir = tempdir();
        save_token_in(&dir, "12th", "sk-xyz").unwrap();
        let raw = std::fs::read_to_string(dir.join("12th.yaml")).unwrap();
        assert!(raw.contains("token:"), "on-disk format is yaml: {raw}");
        assert_eq!(
            load_token_in(&dir, "12th").unwrap(),
            Some("sk-xyz".to_string())
        );
    }

    #[test]
    fn yaml_round_trips_special_chars() {
        let dir = tempdir();
        let token = r#"sk-a:b"c\d e"#;
        save_token_in(&dir, "p", token).unwrap();
        assert_eq!(load_token_in(&dir, "p").unwrap(), Some(token.to_string()));
    }

    #[test]
    fn lists_and_removes_saved_tokens() {
        let dir = tempdir();
        save_token_in(&dir, "openai", "sk-openai").unwrap();
        save_token_in(&dir, "anthropic", "sk-anthropic").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignore").unwrap();

        assert_eq!(
            list_tokens_in(&dir).unwrap(),
            vec!["anthropic".to_string(), "openai".to_string()]
        );
        assert!(remove_token_in(&dir, "openai").unwrap());
        assert_eq!(load_token_in(&dir, "openai").unwrap(), None);
        assert!(!remove_token_in(&dir, "missing").unwrap());
        assert_eq!(list_tokens_in(&dir).unwrap(), vec!["anthropic".to_string()]);
    }

    #[test]
    fn oauth_bundle_round_trips_and_exposes_access_token() {
        let dir = tempdir();
        let cred = AuthCredential::OAuth(OAuthCredential {
            oauth_type: OAuthType::OpenaiCodex,
            access_token: "access-abc".to_string(),
            refresh_token: "refresh-xyz".to_string(),
            expires_at: "2026-07-22T12:00:00Z".to_string(),
            account_id: Some("acct-1".to_string()),
            id_token: Some("id.jwt".to_string()),
        });
        save_credential_in(&dir, "codex", &cred).unwrap();
        assert_eq!(
            load_token_in(&dir, "codex").unwrap(),
            Some("access-abc".to_string())
        );
        let loaded = load_credential_in(&dir, "codex").unwrap().unwrap();
        assert_eq!(loaded, cred);
        let raw = std::fs::read_to_string(dir.join("codex.yaml")).unwrap();
        assert!(raw.contains("type: oauth") || raw.contains("type:oauth"));
        assert!(raw.contains("openai-codex"));
    }

    #[test]
    fn hand_written_token_only_yaml_loads() {
        let dir = tempdir();
        std::fs::write(dir.join("hand.yaml"), "token: \"sk-hand\"\n").unwrap();
        assert_eq!(
            load_token_in(&dir, "hand").unwrap(),
            Some("sk-hand".to_string())
        );
        assert!(matches!(
            load_credential_in(&dir, "hand").unwrap(),
            Some(AuthCredential::Api { token }) if token == "sk-hand"
        ));
    }

    #[test]
    fn incomplete_or_malformed_credential_is_an_error() {
        let dir = tempdir();
        std::fs::write(dir.join("broken.yaml"), "type: oauth\ntoken: x\n").unwrap();
        let error = load_credential_in(&dir, "broken").unwrap_err();
        assert!(error.to_string().contains("missing oauth_type"), "{error}");
        std::fs::write(dir.join("malformed.yaml"), "token: [\n").unwrap();
        assert!(load_credential_in(&dir, "malformed").is_err());
    }

    #[test]
    fn oauth_type_parse_accepts_canonical_ids_only() {
        assert_eq!(
            OAuthType::parse("openai-codex"),
            Some(OAuthType::OpenaiCodex)
        );
        assert_eq!(OAuthType::parse("grok-build"), Some(OAuthType::GrokBuild));
        assert_eq!(OAuthType::parse("codex"), None);
        assert_eq!(OAuthType::parse("grok"), None);
    }
}
