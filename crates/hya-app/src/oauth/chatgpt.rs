//! Official Sign in with ChatGPT: dynamic public-client registration and plan use.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{OAuthError, expires_at_from_secs, json_str};
use crate::auth::{ChatgptRegistration, OAuthCredential, OAuthType};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const JWKS: &str = "https://auth.openai.com/.well-known/jwks.json";
/// Public resource audience for ChatGPT plan usage.
pub const RESOURCE: &str = "https://api.openai.com/v1";
const DIRECT: &str = "chatgpt.tokens.use.direct";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DYNAMIC: &str = "dynamic_agent_client";
const CALLBACK: &str = "/auth/callback";

fn protocol(message: &str) -> OAuthError {
    OAuthError::Protocol(message.into())
}
fn client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| OAuthError::Network(e.to_string()))
}
fn random() -> Result<String, OAuthError> {
    use base64::Engine as _;
    use rand_core::{OsRng, TryRngCore as _};
    let mut bytes = [0; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| protocol("system entropy unavailable"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Metadata survives logout and a failed code exchange without replacing active tokens.
#[derive(Clone, Serialize, Deserialize)]
struct Registration {
    client_id: String,
    identity: Option<ChatgptRegistration>,
}

struct CallbackTask(tokio::task::JoinHandle<Result<HashMap<String, String>, OAuthError>>);
impl Drop for CallbackTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A single expiring login attempt. Secrets never enter the server event log.
pub struct ChatgptLogin {
    /// Browser URL for this login attempt.
    pub authorization_url: String,
    /// One-time OAuth state bound to the attempt.
    pub state: String,
    /// Monotonic deadline for consuming the attempt.
    pub expires: Instant,
    dir: PathBuf,
    provider: String,
    host: String,
    registration: Option<Registration>,
    nonce: String,
    verifier: String,
    redirect: String,
    callback: CallbackTask,
}

impl ChatgptLogin {
    /// Bind the callback before constructing an authorization URL.
    pub async fn start(dir: &Path, provider: &str, timeout: Duration) -> Result<Self, OAuthError> {
        super::validate_provider_id(provider)?;
        std::fs::create_dir_all(dir)?;
        let host_path = dir.join(".chatgpt-host.json");
        let host = match std::fs::read_to_string(&host_path) {
            Ok(s) => serde_json::from_str::<String>(&s)
                .map_err(|_| protocol("invalid ChatGPT host identity"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // UUID v4, generated once per installation; provider IDs are account labels.
                use rand_core::{OsRng, TryRngCore as _};
                let mut b = [0u8; 16];
                OsRng
                    .try_fill_bytes(&mut b)
                    .map_err(|_| protocol("system entropy unavailable"))?;
                b[6] = (b[6] & 15) | 64;
                b[8] = (b[8] & 63) | 128;
                let h = b.iter().map(|v| format!("{v:02x}")).collect::<String>();
                let host = format!(
                    "urn:uuid:{}-{}-{}-{}-{}",
                    &h[..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..]
                );
                // Exclusive creation prevents parallel initial logins inventing two host IDs.
                use std::io::Write as _;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    options.mode(0o600);
                }
                match options.open(&host_path) {
                    Ok(mut f) => {
                        f.write_all(
                            serde_json::to_string(&host)
                                .map_err(|_| protocol("host serialization failed"))?
                                .as_bytes(),
                        )?;
                        host
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        serde_json::from_str(&std::fs::read_to_string(&host_path)?)
                            .map_err(|_| protocol("invalid ChatGPT host identity"))?
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        };
        let registration = read_registration(dir, provider)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://127.0.0.1:{}{CALLBACK}",
            listener.local_addr()?.port()
        );
        let state = random()?;
        let nonce = random()?;
        let verifier = random()?;
        use base64::Engine as _;
        use sha2::{Digest as _, Sha256};
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let mut url =
            reqwest::Url::parse(AUTHORIZE).map_err(|_| protocol("invalid authorize endpoint"))?;
        url.query_pairs_mut().extend_pairs([
            (
                "client_id",
                registration
                    .as_ref()
                    .map_or(DYNAMIC, |r| r.client_id.as_str()),
            ),
            ("ext_agent_host_id", host.as_str()),
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
        ]);
        if registration.is_none() {
            url.query_pairs_mut().append_pair("agent_name_hint", "Hya");
        }
        // Do not put a retained ID token in a displayed URL or frontend state.
        let callback_redirect = redirect.clone();
        let callback_state = state.clone();
        let callback = CallbackTask(tokio::spawn(async move {
            tokio::time::timeout(
                timeout,
                wait_callback(listener, &callback_redirect, &callback_state),
            )
            .await
            .map_err(|_| OAuthError::Timeout("ChatGPT sign-in expired".into()))?
        }));
        Ok(Self {
            authorization_url: url.into(),
            state,
            expires: Instant::now() + timeout,
            dir: dir.into(),
            provider: provider.into(),
            host,
            registration,
            nonce,
            verifier,
            redirect,
            callback,
        })
    }

    /// Wait for the loopback callback, or accept its full redirected URL for SSH use.
    pub async fn finish(self, callback_url: &str) -> Result<OAuthCredential, OAuthError> {
        self.finish_at(callback_url, TOKEN, JWKS).await
    }
    async fn finish_at(
        mut self,
        callback_url: &str,
        token_url: &str,
        jwks_url: &str,
    ) -> Result<OAuthCredential, OAuthError> {
        let remaining = self.expires.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(OAuthError::Timeout("ChatGPT sign-in expired".into()));
        }
        let params = if callback_url.is_empty() {
            tokio::time::timeout(remaining, &mut self.callback.0)
                .await
                .map_err(|_| OAuthError::Timeout("ChatGPT sign-in expired; start again".into()))?
                .map_err(|_| protocol("ChatGPT callback task stopped"))??
        } else {
            self.parse_callback(callback_url)?
        };
        if params.get("state") != Some(&self.state) {
            return Err(protocol("ChatGPT callback state mismatch"));
        }
        if params.contains_key("error") {
            return Err(protocol(
                "ChatGPT authorization was denied; no credentials changed",
            ));
        }
        let code = params
            .get("code")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| protocol("ChatGPT callback missing code"))?;
        let issued = issued_client(
            &params,
            self.registration.as_ref().map(|r| r.client_id.as_str()),
        )?;
        let identity = self.registration.as_ref().and_then(|r| r.identity.clone());
        write_registration(
            &self.dir,
            &self.provider,
            &Registration {
                client_id: issued.clone(),
                identity: identity.clone(),
            },
        )?;
        let c = client()?;
        let tokens = token_form_at(
            &c,
            token_url,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &issued),
                ("code", code),
                ("code_verifier", &self.verifier),
                ("redirect_uri", &self.redirect),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let id_token = json_str(&tokens, "id_token")
            .ok_or_else(|| protocol("ChatGPT token response missing ID token"))?;
        let keys: JwkSet = c
            .get(jwks_url)
            .send()
            .await
            .map_err(|_| protocol("could not fetch OpenAI signing keys"))?
            .error_for_status()
            .map_err(|_| protocol("OpenAI signing keys unavailable"))?
            .json()
            .await
            .map_err(|_| protocol("invalid OpenAI signing keys"))?;
        let claims = verify_identity(id_token, &issued, Some(&self.nonce), &keys)?;
        if identity
            .as_ref()
            .is_some_and(|old| old.subject != claims.sub || old.client_id != issued)
        {
            return Err(protocol(
                "ChatGPT account differs from this registration; use another provider name",
            ));
        }
        let registration = ChatgptRegistration {
            client_id: issued,
            host_id: self.host,
            subject: claims.sub,
            email: claims.email,
            scopes: granted_scopes(&tokens)?,
        };
        let credential = credential(&tokens, registration.clone(), None)?;
        write_registration(
            &self.dir,
            &self.provider,
            &Registration {
                client_id: registration.client_id.clone(),
                identity: Some(registration),
            },
        )?;
        Ok(credential)
    }

    fn parse_callback(&self, url: &str) -> Result<HashMap<String, String>, OAuthError> {
        parse_callback_url(url, &self.redirect)
    }
}

fn parse_callback_url(
    url: &str,
    redirect_uri: &str,
) -> Result<HashMap<String, String>, OAuthError> {
    let u =
        reqwest::Url::parse(url).map_err(|_| protocol("paste the full redirected callback URL"))?;
    let redirect =
        reqwest::Url::parse(redirect_uri).map_err(|_| protocol("invalid pending callback"))?;
    if u.origin() != redirect.origin()
        || u.path() != CALLBACK
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(protocol("callback URL does not match this ChatGPT login"));
    }
    let mut params = HashMap::new();
    for (k, v) in u.query_pairs() {
        if params.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(protocol("duplicate callback parameter"));
        }
    }
    Ok(params)
}

async fn wait_callback(
    listener: TcpListener,
    redirect: &str,
    state: &str,
) -> Result<HashMap<String, String>, OAuthError> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = [0u8; 8192];
        let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut bytes)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };
        let request = String::from_utf8_lossy(&bytes[..n]);
        let line = request.lines().next().unwrap_or("");
        let target = line
            .strip_prefix("GET ")
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("/");
        let url = format!("http://127.0.0.1:{}{target}", listener.local_addr()?.port());
        let params = parse_callback_url(&url, redirect);
        let valid = params
            .as_ref()
            .is_ok_and(|p| p.get("state") == Some(&state.to_owned()));
        let response = if valid {
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nReturn to Hya to finish connecting."
        } else {
            "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nInvalid Hya callback."
        };
        let _ = stream.write_all(response.as_bytes()).await;
        if valid {
            return params;
        }
    }
}

fn registration_path(dir: &Path, provider: &str) -> PathBuf {
    dir.join(format!(".{provider}.chatgpt.json"))
}
fn read_registration(dir: &Path, provider: &str) -> Result<Option<Registration>, OAuthError> {
    match std::fs::read(registration_path(dir, provider)) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|_| protocol("invalid ChatGPT registration")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(crate::auth::load_credential_in(dir, provider)
                .and_then(|c| c.oauth().and_then(|o| o.chatgpt.as_deref().cloned()))
                .map(|identity| Registration {
                    client_id: identity.client_id.clone(),
                    identity: Some(identity),
                }))
        }
        Err(e) => Err(e.into()),
    }
}
fn write_registration(
    dir: &Path,
    provider: &str,
    registration: &Registration,
) -> Result<(), OAuthError> {
    let path = registration_path(dir, provider);
    crate::auth::write_secret(
        &path,
        &serde_json::to_string(registration)
            .map_err(|_| protocol("registration serialization failed"))?,
    )?;
    Ok(())
}
fn issued_client(
    params: &HashMap<String, String>,
    old: Option<&str>,
) -> Result<String, OAuthError> {
    match (old, params.get("client_id").map(String::as_str)) {
        (Some(old), None) => Ok(old.into()),
        (Some(old), Some(new)) if old == new => Ok(old.into()),
        (None, Some(new)) if !new.is_empty() && new != DYNAMIC => Ok(new.into()),
        _ => Err(protocol("missing or mismatched issued ChatGPT client ID")),
    }
}
#[derive(Deserialize)]
struct Identity {
    sub: String,
    nonce: Option<String>,
    email: Option<String>,
}
fn verify_identity(
    token: &str,
    client: &str,
    nonce: Option<&str>,
    keys: &JwkSet,
) -> Result<Identity, OAuthError> {
    let header = decode_header(token).map_err(|_| protocol("invalid ChatGPT ID token"))?;
    if header.alg != Algorithm::RS256 {
        return Err(protocol("unsupported ChatGPT ID-token algorithm"));
    }
    let key = header
        .kid
        .as_deref()
        .and_then(|kid| keys.find(kid))
        .ok_or_else(|| protocol("unknown ChatGPT signing key"))?;
    let key = DecodingKey::from_jwk(key).map_err(|_| protocol("invalid ChatGPT signing key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client]);
    validation.leeway = 5;
    validation.set_required_spec_claims(&["sub", "exp", "iat", "iss", "aud"]);
    let identity = decode::<Identity>(token, &key, &validation)
        .map_err(|_| protocol("ChatGPT ID-token verification failed"))?
        .claims;
    if nonce.is_some_and(|expected| identity.nonce.as_deref() != Some(expected))
        || identity.sub.is_empty()
    {
        return Err(protocol("ChatGPT identity nonce or subject mismatch"));
    }
    Ok(identity)
}
fn granted_scopes(tokens: &Value) -> Result<Vec<String>, OAuthError> {
    let scopes: Vec<String> = json_str(tokens, "scope")
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if !scopes.iter().any(|s| s == DIRECT) || !scopes.iter().any(|s| s == "resource.invoke") {
        return Err(protocol(
            "ChatGPT plan usage was not granted; authorize plan usage and try again",
        ));
    }
    Ok(scopes)
}
async fn token_form(c: &reqwest::Client, form: &[(&str, &str)]) -> Result<Value, OAuthError> {
    token_form_at(c, TOKEN, form).await
}
async fn token_form_at(
    c: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<Value, OAuthError> {
    let response = c
        .post(endpoint)
        .form(form)
        .send()
        .await
        .map_err(|_| OAuthError::Network("ChatGPT token endpoint unavailable".into()))?;
    if !response.status().is_success() {
        return Err(OAuthError::needs_login(
            "chatgpt",
            OAuthType::OpenaiChatgpt,
            "ChatGPT token exchange rejected; reconnect this account",
        ));
    }
    response
        .json()
        .await
        .map_err(|_| protocol("invalid ChatGPT token response"))
}
fn credential(
    tokens: &Value,
    registration: ChatgptRegistration,
    previous: Option<&OAuthCredential>,
) -> Result<OAuthCredential, OAuthError> {
    let access = json_str(tokens, "access_token")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| protocol("missing ChatGPT access token"))?;
    let refresh = json_str(tokens, "refresh_token")
        .filter(|s| !s.is_empty())
        .or_else(|| previous.map(|p| p.refresh_token.as_str()))
        .ok_or_else(|| protocol("missing ChatGPT refresh token"))?;
    if json_str(tokens, "token_type").is_some_and(|v| !v.eq_ignore_ascii_case("Bearer")) {
        return Err(protocol("unsupported ChatGPT token type"));
    }
    Ok(OAuthCredential {
        oauth_type: OAuthType::OpenaiChatgpt,
        chatgpt: Some(Box::new(registration)),
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: expires_at_from_secs(
            tokens
                .get("expires_in")
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )?,
        account_id: None,
        id_token: json_str(tokens, "id_token")
            .map(str::to_string)
            .or_else(|| previous.and_then(|p| p.id_token.clone())),
    })
}
/// Validate saved plan permission before resolving a fresh bearer as well as refreshing.
pub(crate) fn validate_saved(credential: &OAuthCredential) -> Result<(), OAuthError> {
    let valid = credential.chatgpt.as_ref().is_some_and(|r| {
        !r.client_id.is_empty()
            && r.client_id != DYNAMIC
            && !r.subject.is_empty()
            && r.scopes.iter().any(|s| s == DIRECT)
            && r.scopes.iter().any(|s| s == "resource.invoke")
    });
    if !valid {
        return Err(OAuthError::needs_login(
            "chatgpt",
            OAuthType::OpenaiChatgpt,
            "missing verified registration or ChatGPT plan permission",
        ));
    }
    Ok(())
}

/// Refresh with the issued client ID and replace rotated tokens together.
pub async fn refresh_chatgpt(previous: &OAuthCredential) -> Result<OAuthCredential, OAuthError> {
    validate_saved(previous)?;
    let mut registration = previous
        .chatgpt
        .clone()
        .ok_or_else(|| protocol("missing ChatGPT registration; reconnect"))?;
    let tokens = token_form(
        &client()?,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &registration.client_id),
            ("refresh_token", &previous.refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await?;
    if let Some(id_token) = json_str(&tokens, "id_token") {
        let keys: JwkSet = client()?
            .get(JWKS)
            .send()
            .await
            .map_err(|_| protocol("OpenAI signing keys unavailable"))?
            .error_for_status()
            .map_err(|_| protocol("OpenAI signing keys unavailable"))?
            .json()
            .await
            .map_err(|_| protocol("invalid OpenAI signing keys"))?;
        let identity = verify_identity(id_token, &registration.client_id, None, &keys)?;
        if identity.sub != registration.subject {
            return Err(protocol("ChatGPT refresh changed account identity"));
        }
    }
    if tokens.get("scope").is_some() {
        registration.scopes = granted_scopes(&tokens)?;
    }
    if !registration.scopes.iter().any(|s| s == DIRECT) {
        return Err(protocol("ChatGPT plan usage is not authorized"));
    }
    credential(&tokens, *registration, Some(previous))
}

/// Cross-process single flight for rotating ChatGPT refresh tokens. Closing the
/// returned file releases the kernel lock, including after process termination.
pub(crate) async fn lock_refresh(dir: &Path, provider: &str) -> Result<std::fs::File, OAuthError> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!(".{provider}.chatgpt-refresh.lock"));
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: the descriptor is owned by file for this whole call.
            while unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(OAuthError::Io(error));
                }
            }
        }
        Ok(file)
    })
    .await
    .map_err(|_| protocol("ChatGPT refresh lock task failed"))?
}

/// End the renewable session when possible, then clear local tokens while
/// retaining the account registration and stable host identity.
pub async fn logout_in(dir: &Path, provider: &str) -> Result<(bool, Option<String>), OAuthError> {
    super::validate_provider_id(provider)?;
    let _guard = lock_refresh(dir, provider).await?;
    let credential = crate::auth::load_credential_in(dir, provider);
    let warning = if let Some(crate::auth::AuthCredential::OAuth(oauth)) = credential
        && oauth.oauth_type == OAuthType::OpenaiChatgpt
    {
        revoke(&oauth).await.err().map(|_| "Signed out locally; remote revocation was not confirmed. Disconnect Hya in https://chatgpt.com/settings/usage".into())
    } else {
        None
    };
    Ok((crate::auth::remove_token_in(dir, provider)?, warning))
}
async fn revoke(credential: &OAuthCredential) -> Result<(), OAuthError> {
    let registration = credential
        .chatgpt
        .as_ref()
        .ok_or_else(|| protocol("missing ChatGPT registration"))?;
    let c = client()?;
    let discovery: Value = c
        .get("https://auth.openai.com/.well-known/openid-configuration")
        .send()
        .await
        .map_err(|_| protocol("revocation discovery unavailable"))?
        .error_for_status()
        .map_err(|_| protocol("revocation discovery failed"))?
        .json()
        .await
        .map_err(|_| protocol("invalid revocation discovery"))?;
    let endpoint = json_str(&discovery, "revocation_endpoint")
        .and_then(|s| reqwest::Url::parse(s).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("auth.openai.com")
                && url.username().is_empty()
                && url.password().is_none()
        })
        .ok_or_else(|| protocol("untrusted revocation endpoint"))?;
    for attempt in 0..2 {
        match c
            .post(endpoint.clone())
            .form(&[
                ("token", credential.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", registration.client_id.as_str()),
            ])
            .send()
            .await
        {
            Ok(r) if r.status() == reqwest::StatusCode::OK => return Ok(()),
            Ok(r) if !r.status().is_server_error() => return Err(protocol("revocation rejected")),
            _ if attempt == 0 => tokio::time::sleep(Duration::from_millis(200)).await,
            _ => return Err(protocol("revocation unavailable")),
        }
    }
    Err(protocol("revocation unavailable"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        serde_json::from_str(include_str!("chatgpt_fixture.json")).unwrap()
    }
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!("hya-chatgpt-{}", random().unwrap()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn signature_audience_expiry_and_nonce_are_verified() {
        let f = fixture();
        let keys = serde_json::from_value(f["jwks"].clone()).unwrap();
        let good = f["valid"].as_str().unwrap();
        assert_eq!(
            verify_identity(good, "oaiapp_hya_test", Some("test-nonce"), &keys)
                .unwrap()
                .sub,
            "test-subject"
        );
        assert!(verify_identity(good, "other-client", Some("test-nonce"), &keys).is_err());
        assert!(verify_identity(good, "oaiapp_hya_test", Some("wrong-nonce"), &keys).is_err());
        assert!(
            verify_identity(
                f["expired"].as_str().unwrap(),
                "oaiapp_hya_test",
                Some("test-nonce"),
                &keys
            )
            .is_err()
        );
        let (body, sig) = good.rsplit_once('.').unwrap();
        let damaged = format!(
            "{body}.{}{}",
            if sig.starts_with('a') { "b" } else { "a" },
            &sig[1..]
        );
        assert!(verify_identity(&damaged, "oaiapp_hya_test", Some("test-nonce"), &keys).is_err());
    }
    #[test]
    fn registration_ids_and_grants_cannot_be_substituted() {
        let mut params = HashMap::new();
        assert!(issued_client(&params, None).is_err());
        params.insert("client_id".into(), DYNAMIC.into());
        assert!(issued_client(&params, None).is_err());
        params.insert("client_id".into(), "issued".into());
        assert_eq!(issued_client(&params, None).unwrap(), "issued");
        assert!(issued_client(&params, Some("different")).is_err());
        params.clear();
        assert_eq!(issued_client(&params, Some("issued")).unwrap(), "issued");
        assert!(granted_scopes(&json!({"scope":"openid profile email"})).is_err());
        assert!(granted_scopes(&json!({"scope":SCOPES})).is_ok());
    }
    #[tokio::test]
    async fn host_and_registration_survive_retry_and_callback_is_bound_to_attempt() {
        let dir = temp();
        let a = ChatgptLogin::start(&dir, "personal", Duration::from_secs(5))
            .await
            .unwrap();
        let url = reqwest::Url::parse(&a.authorization_url).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], DYNAMIC);
        assert_eq!(query["agent_name_hint"], "Hya");
        assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
        let host = query["ext_agent_host_id"].clone();
        write_registration(
            &dir,
            "personal",
            &Registration {
                client_id: "oaiapp_saved".into(),
                identity: None,
            },
        )
        .unwrap();
        let b = ChatgptLogin::start(&dir, "personal", Duration::from_secs(5))
            .await
            .unwrap();
        let q: HashMap<_, _> = reqwest::Url::parse(&b.authorization_url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(q["client_id"], "oaiapp_saved");
        assert_eq!(q["ext_agent_host_id"], host);
        assert!(!q.contains_key("agent_name_hint"));
        assert_ne!(a.state, b.state);
        assert!(
            b.parse_callback("http://example.com/auth/callback?code=x")
                .is_err()
        );
        assert!(
            b.parse_callback(&format!("{}?code=x&state=a&state=b", b.redirect))
                .is_err()
        );
        let bad = format!("{}?code=x&state=wrong", b.redirect);
        assert!(b.finish(&bad).await.is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(dir.join(".chatgpt-host.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(registration_path(&dir, "personal"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn full_code_exchange_validates_identity_and_retains_registration_not_tokens_on_failure()
    {
        use axum::{
            Json, Router,
            extract::Form,
            routing::{get, post},
        };
        let dir = temp();
        let f = fixture();
        let mut pending = ChatgptLogin::start(&dir, "personal", Duration::from_secs(5))
            .await
            .unwrap();
        pending.nonce = "test-nonce".into();
        let token = f["valid"].clone();
        let keys = f["jwks"].clone();
        let app=Router::new().route("/token",post(move |Form(form):Form<HashMap<String,String>>| {
            let token=token.clone();async move {
                assert_eq!(form["client_id"],"oaiapp_hya_test");assert_eq!(form["code"],"returned-code");
                assert_eq!(form["resource"],RESOURCE);assert_eq!(form["grant_type"],"authorization_code");
                Json(json!({"access_token":"access-fixture","refresh_token":"refresh-fixture","id_token":token,"expires_in":3600,"scope":SCOPES,"token_type":"Bearer"}))
            }
        })).route("/jwks",get(move || {let keys=keys.clone();async move {Json(keys)}}));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let callback = format!(
            "{}?code=returned-code&state={}&client_id=oaiapp_hya_test",
            pending.redirect, pending.state
        );
        let cred = pending
            .finish_at(
                &callback,
                &format!("http://{addr}/token"),
                &format!("http://{addr}/jwks"),
            )
            .await
            .unwrap();
        assert_eq!(cred.oauth_type, OAuthType::OpenaiChatgpt);
        assert_eq!(cred.chatgpt.as_ref().unwrap().subject, "test-subject");
        crate::auth::save_credential_in(
            &dir,
            "personal",
            &crate::auth::AuthCredential::OAuth(cred.clone()),
        )
        .unwrap();
        let loaded = crate::auth::load_credential_in(&dir, "personal").unwrap();
        assert_eq!(loaded.oauth(), Some(&cred));
        let again = ChatgptLogin::start(&dir, "personal", Duration::from_secs(5))
            .await
            .unwrap();
        let callback = format!(
            "{}?error=access_denied&state={}",
            again.redirect, again.state
        );
        assert!(
            again
                .finish_at(
                    &callback,
                    &format!("http://{addr}/token"),
                    &format!("http://{addr}/jwks")
                )
                .await
                .is_err()
        );
        assert_eq!(
            crate::auth::load_credential_in(&dir, "personal")
                .unwrap()
                .oauth(),
            Some(&cred)
        );
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn fresh_credentials_still_require_the_saved_plan_grant() {
        let dir = temp();
        let registration = ChatgptRegistration {
            client_id: "issued".into(),
            host_id: "host".into(),
            subject: "subject".into(),
            email: None,
            scopes: SCOPES.split_whitespace().map(str::to_string).collect(),
        };
        let mut oauth = credential(
            &json!({"access_token":"fresh","refresh_token":"r","expires_in":3600}),
            registration,
            None,
        )
        .unwrap();
        crate::auth::save_credential_in(
            &dir,
            "personal",
            &crate::auth::AuthCredential::OAuth(oauth.clone()),
        )
        .unwrap();
        assert_eq!(
            super::super::ensure_access_token_in(&dir, "personal").unwrap(),
            "fresh"
        );
        oauth.chatgpt.as_mut().unwrap().scopes = vec!["openid".into()];
        crate::auth::save_credential_in(
            &dir,
            "personal",
            &crate::auth::AuthCredential::OAuth(oauth),
        )
        .unwrap();
        assert!(super::super::ensure_access_token_in(&dir, "personal").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn logout_clears_local_tokens_and_warns_when_revocation_cannot_be_confirmed() {
        let dir = temp();
        // Missing registration fails before any network request, exercising the
        // local sign-out guarantee even when revocation is impossible.
        let oauth = OAuthCredential {
            oauth_type: OAuthType::OpenaiChatgpt,
            chatgpt: None,
            access_token: "test".into(),
            refresh_token: "refresh".into(),
            expires_at: expires_at_from_secs(3600).unwrap(),
            account_id: None,
            id_token: None,
        };
        crate::auth::save_credential_in(
            &dir,
            "personal",
            &crate::auth::AuthCredential::OAuth(oauth),
        )
        .unwrap();
        let (removed, warning) = logout_in(&dir, "personal").await.unwrap();
        assert!(removed);
        assert!(
            warning
                .unwrap()
                .contains("remote revocation was not confirmed")
        );
        assert!(crate::auth::load_credential_in(&dir, "personal").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn refresh_preserves_registration_and_rotated_credentials() {
        let registration = ChatgptRegistration {
            client_id: "issued".into(),
            host_id: "host".into(),
            subject: "subject".into(),
            email: None,
            scopes: SCOPES.split_whitespace().map(str::to_string).collect(),
        };
        let old = credential(
            &json!({"access_token":"old","refresh_token":"r1","expires_in":3600}),
            registration.clone(),
            None,
        )
        .unwrap();
        let new = credential(
            &json!({"access_token":"new","refresh_token":"r2","expires_in":3600}),
            registration.clone(),
            Some(&old),
        )
        .unwrap();
        assert_eq!(new.refresh_token, "r2");
        assert_eq!(new.chatgpt, old.chatgpt);
        assert_eq!(
            credential(
                &json!({"access_token":"new","expires_in":3600}),
                registration,
                Some(&old)
            )
            .unwrap()
            .refresh_token,
            "r1"
        );
    }
}
