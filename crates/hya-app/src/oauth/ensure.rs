//! Load OAuth credentials and refresh access tokens when near expiry.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Mutex;

use hya_provider::ProviderAuthFlow;

use crate::auth::{AuthCredential, OAuthCredential, load_credential_in, save_credential_in};

use super::{OAuthError, is_expired};

const DEFAULT_SKEW: Duration = Duration::from_secs(5 * 60);

fn refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Non-secret status line for `oauth status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthStatus {
    pub provider: String,
    pub kind: String,
    pub oauth_type: Option<String>,
    pub expires_at: Option<String>,
    pub expired: bool,
    pub account_id: Option<String>,
}

/// Ensure a valid access token for `provider` in the user auth dir,
/// refreshing through `flow` if needed.
pub fn ensure_access_token(
    provider: &str,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    let dir =
        crate::auth::auth_dir().ok_or_else(|| OAuthError::Config("no config directory".into()))?;
    ensure_access_token_in(&dir, provider, flow)
}

/// Ensure a valid access token under an explicit auth directory.
pub fn ensure_access_token_in(
    dir: &Path,
    provider: &str,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    // Runtime path is sync (BearerResolver); block on refresh via current runtime or new one.
    pollster_or_block(ensure_access_token_in_async_ref(dir, provider, flow))
}

/// Force-refresh `provider`'s access token, bypassing the expiry skew check.
///
/// Entry point for the 401/403 auth-recovery level: the upstream just rejected
/// the credential we resolved, so freshness by local clock no longer matters.
/// Single-flight and rotated-refresh-token safe — see [`force_refresh_with`].
pub fn force_refresh_access_token(
    provider: &str,
    stale_access_token: &str,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    let dir =
        crate::auth::auth_dir().ok_or_else(|| OAuthError::Config("no config directory".into()))?;
    force_refresh_access_token_in(&dir, provider, stale_access_token, flow)
}

/// [`force_refresh_access_token`] under an explicit auth directory.
pub fn force_refresh_access_token_in(
    dir: &Path,
    provider: &str,
    stale_access_token: &str,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    pollster_or_block(force_refresh_with(
        dir,
        provider,
        stale_access_token,
        |latest| async move { refresh_credential(provider, &latest, flow).await },
    ))
}

async fn ensure_access_token_in_async_ref(
    dir: &Path,
    provider: &str,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    let cred = load_credential_in(dir, provider)?.ok_or_else(|| OAuthError::NeedsLogin {
        provider: provider.to_string(),
        oauth_type: "unknown".into(),
        reason: "no saved credentials".into(),
    })?;

    match cred {
        AuthCredential::Api { token } => Ok(token),
        AuthCredential::OAuth(oauth) => {
            if !is_expired(&oauth.expires_at, DEFAULT_SKEW)? {
                return Ok(oauth.access_token);
            }
            refresh_and_store(dir, provider, oauth, flow).await
        }
    }
}

async fn refresh_and_store(
    dir: &Path,
    provider: &str,
    oauth: OAuthCredential,
    flow: &dyn ProviderAuthFlow,
) -> Result<String, OAuthError> {
    // Single-flight process lock so concurrent streams don't burn a rotated refresh token.
    let _guard = refresh_lock().lock().await;

    // Re-read: another task may have refreshed while we waited.
    if let Some(AuthCredential::OAuth(latest)) = load_credential_in(dir, provider)? {
        if !is_expired(&latest.expires_at, DEFAULT_SKEW).unwrap_or(true) {
            return Ok(latest.access_token.clone());
        }
        let refreshed = refresh_credential(provider, &latest, flow).await?;
        save_credential_in(dir, provider, &AuthCredential::OAuth(refreshed.clone()))?;
        return Ok(refreshed.access_token);
    }
    let refreshed = refresh_credential(provider, &oauth, flow).await?;
    save_credential_in(dir, provider, &AuthCredential::OAuth(refreshed.clone()))?;
    Ok(refreshed.access_token)
}

async fn force_refresh_with<F, Fut>(
    dir: &Path,
    provider: &str,
    stale_access_token: &str,
    network_refresh: F,
) -> Result<String, OAuthError>
where
    F: Fn(OAuthCredential) -> Fut,
    Fut: std::future::Future<Output = Result<OAuthCredential, OAuthError>>,
{
    let cred = load_credential_in(dir, provider)?.ok_or_else(|| OAuthError::NeedsLogin {
        provider: provider.to_string(),
        oauth_type: "unknown".into(),
        reason: "no saved credentials".into(),
    })?;
    let oauth = match cred {
        AuthCredential::Api { token } => return Ok(token),
        AuthCredential::OAuth(oauth) => oauth,
    };

    // Same single-flight process lock as refresh_and_store so concurrent
    // streams recovering from 401 cannot burn a rotated refresh token.
    let _guard = refresh_lock().lock().await;

    // Re-read past the lock. Unlike refresh_and_store the comparison is not a
    // skew check but token identity: when storage already moved past the exact
    // access token that failed upstream, some other stream refreshed for us,
    // so skip the network refresh entirely and reuse its result.
    if let Some(AuthCredential::OAuth(latest)) = load_credential_in(dir, provider)? {
        if latest.access_token != stale_access_token {
            return Ok(latest.access_token);
        }
        let refreshed = network_refresh(latest).await?;
        save_credential_in(dir, provider, &AuthCredential::OAuth(refreshed.clone()))?;
        return Ok(refreshed.access_token);
    }
    let refreshed = network_refresh(oauth).await?;
    save_credential_in(dir, provider, &AuthCredential::OAuth(refreshed.clone()))?;
    Ok(refreshed.access_token)
}

async fn refresh_credential(
    provider: &str,
    oauth: &OAuthCredential,
    flow: &dyn ProviderAuthFlow,
) -> Result<OAuthCredential, OAuthError> {
    match flow.refresh(oauth).await {
        Ok(mut cred) => {
            // Preserve account_id if refresh response omitted id_token claims.
            if cred.account_id.is_none() {
                cred.account_id = oauth.account_id.clone();
            }
            Ok(cred)
        }
        Err(OAuthError::NeedsLogin {
            oauth_type, reason, ..
        }) => Err(OAuthError::NeedsLogin {
            provider: provider.to_string(),
            oauth_type,
            reason,
        }),
        Err(OAuthError::Entitlement { detail, .. }) => Err(OAuthError::Entitlement {
            provider: provider.to_string(),
            detail,
        }),
        Err(other) => Err(other),
    }
}

/// Status snapshot for one or all providers under `dir`.
pub fn oauth_status_in(dir: &Path, provider: Option<&str>) -> Result<Vec<OAuthStatus>, OAuthError> {
    let providers = if let Some(p) = provider {
        vec![p.to_string()]
    } else {
        crate::auth::list_tokens_in(dir)?
    };
    let mut out = Vec::new();
    for p in providers {
        let Some(cred) = load_credential_in(dir, &p)? else {
            continue;
        };
        match cred {
            AuthCredential::Api { .. } => out.push(OAuthStatus {
                provider: p,
                kind: "api".into(),
                oauth_type: None,
                expires_at: None,
                expired: false,
                account_id: None,
            }),
            AuthCredential::OAuth(oauth) => {
                let expired = is_expired(&oauth.expires_at, Duration::from_secs(0)).unwrap_or(true);
                out.push(OAuthStatus {
                    provider: p,
                    kind: "oauth".into(),
                    oauth_type: Some(oauth.oauth_type),
                    expires_at: Some(oauth.expires_at),
                    expired,
                    account_id: oauth.account_id,
                });
            }
        }
    }
    Ok(out)
}

/// Run an async future from a sync context without requiring an existing runtime handle.
fn pollster_or_block<F, T>(fut: F) -> Result<T, OAuthError>
where
    F: std::future::Future<Output = Result<T, OAuthError>>,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| OAuthError::Network(format!("build oauth refresh runtime: {e}")))?;
            rt.block_on(fut)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::auth::{save_credential_in, save_token_in};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A flow whose refresh rotates the tokens without any network.
    struct FakeFlow(&'static str);

    #[async_trait::async_trait]
    impl ProviderAuthFlow for FakeFlow {
        fn oauth_type(&self) -> &str {
            self.0
        }
        fn default_base_url(&self) -> &str {
            "http://127.0.0.1:9/v1"
        }
        fn default_model(&self) -> &str {
            "m"
        }
        async fn login(
            &self,
            _request: hya_provider::OAuthLoginRequest,
        ) -> Result<OAuthCredential, OAuthError> {
            Err(OAuthError::Protocol("no login in tests".into()))
        }
        async fn refresh(
            &self,
            credential: &OAuthCredential,
        ) -> Result<OAuthCredential, OAuthError> {
            Ok(OAuthCredential {
                oauth_type: credential.oauth_type.clone(),
                access_token: format!("{}-refreshed-by-{}", credential.access_token, self.0),
                refresh_token: format!("{}-next", credential.refresh_token),
                expires_at: "2099-01-01T00:00:00Z".into(),
                account_id: None,
                id_token: None,
            })
        }
    }

    fn tempdir() -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hya-oauth-ensure-{}-{}-{}",
            nanos,
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn api_token_returns_without_refresh() {
        let dir = tempdir();
        save_token_in(&dir, "anthropic", "sk-test").unwrap();
        let token = ensure_access_token_in(&dir, "anthropic", &FakeFlow("openai-codex")).unwrap();
        assert_eq!(token, "sk-test");
    }

    #[test]
    fn fresh_oauth_token_returned_as_is() {
        let dir = tempdir();
        let far = time::OffsetDateTime::now_utc() + time::Duration::hours(2);
        let expires = far
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        save_credential_in(
            &dir,
            "codex",
            &AuthCredential::OAuth(OAuthCredential {
                oauth_type: "openai-codex".into(),
                access_token: "fresh-access".into(),
                refresh_token: "rt".into(),
                expires_at: expires,
                account_id: Some("a1".into()),
                id_token: None,
            }),
        )
        .unwrap();
        let token = ensure_access_token_in(&dir, "codex", &FakeFlow("openai-codex")).unwrap();
        assert_eq!(token, "fresh-access");
    }

    #[test]
    fn status_lists_api_and_oauth() {
        let dir = tempdir();
        save_token_in(&dir, "anthropic", "sk").unwrap();
        let far = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        let expires = far
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        save_credential_in(
            &dir,
            "grok",
            &AuthCredential::OAuth(OAuthCredential {
                oauth_type: "grok-build".into(),
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: expires.clone(),
                account_id: None,
                id_token: None,
            }),
        )
        .unwrap();
        let statuses = oauth_status_in(&dir, None).unwrap();
        assert_eq!(statuses.len(), 2);
        let grok = statuses.iter().find(|s| s.provider == "grok").unwrap();
        assert_eq!(grok.kind, "oauth");
        assert_eq!(grok.oauth_type.as_deref(), Some("grok-build"));
        assert!(!grok.expired);
    }

    #[tokio::test]
    async fn concurrent_forced_refreshes_perform_one_network_refresh() {
        let dir = tempdir();
        let expires = (time::OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        save_credential_in(
            &dir,
            "codex",
            &AuthCredential::OAuth(OAuthCredential {
                oauth_type: "openai-codex".into(),
                access_token: "stale-t0".into(),
                refresh_token: "rt-0".into(),
                expires_at: expires,
                account_id: Some("acct-1".into()),
                id_token: None,
            }),
        )
        .unwrap();

        // Eight streams all rejected with 401 against "stale-t0" force a
        // refresh simultaneously: exactly one network refresh may happen and
        // every caller must leave with the same rotated access token.
        let network_calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let task_dir = dir.clone();
            let task_calls = Arc::clone(&network_calls);
            handles.push(tokio::spawn(async move {
                force_refresh_with(&task_dir, "codex", "stale-t0", move |latest| {
                    let calls = Arc::clone(&task_calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Widen the contention window so slower followers must
                        // queue behind the refresh instead of racing past it.
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        Ok::<_, OAuthError>(OAuthCredential {
                            oauth_type: latest.oauth_type,
                            access_token: format!("rotated-{}", latest.access_token),
                            refresh_token: format!("rt-next-{}", latest.refresh_token),
                            expires_at: (time::OffsetDateTime::now_utc()
                                + time::Duration::hours(2))
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap(),
                            account_id: latest.account_id.clone(),
                            id_token: latest.id_token.clone(),
                        })
                    }
                })
                .await
                .expect("forced refresh should succeed for every waiter")
            }));
        }
        let mut tokens = Vec::new();
        for handle in handles {
            tokens.push(handle.await.expect("task join"));
        }

        assert!(tokens.iter().all(|token| token == "rotated-stale-t0"));
        assert_eq!(network_calls.load(Ordering::SeqCst), 1);
        match load_credential_in(&dir, "codex").unwrap() {
            Some(AuthCredential::OAuth(stored)) => {
                assert_eq!(stored.access_token, "rotated-stale-t0")
            }
            other => panic!("expected stored oauth credential, got {other:?}"),
        }
    }

    /// Auth files exactly as 0.51 writes them (`serde_norway` of its
    /// `AuthFile`, OAuth type as a kebab-case scalar).
    const API_051: &str = "type: api\ntoken: sk-051\noauth_type: null\naccess_token: null\nrefresh_token: null\nexpires_at: null\naccount_id: null\nid_token: null\n";
    const CODEX_051: &str = "type: oauth\ntoken: null\noauth_type: openai-codex\naccess_token: codex-access\nrefresh_token: codex-refresh\nexpires_at: 2020-01-01T00:00:00Z\naccount_id: acct-051\nid_token: id.jwt\n";
    const GROK_051: &str = "type: oauth\ntoken: null\noauth_type: grok-build\naccess_token: grok-access\nrefresh_token: grok-refresh\nexpires_at: 2020-01-01T00:00:00Z\naccount_id: null\nid_token: null\n";

    #[test]
    fn stored_051_credentials_load_and_refresh_through_registered_flow() {
        let dir = tempdir();
        for (id, body) in [("api", API_051), ("codex", CODEX_051), ("grok", GROK_051)] {
            std::fs::write(dir.join(format!("{id}.yaml")), body).unwrap();
            // The layout is unchanged: rewriting the loaded credential gives the same bytes.
            let loaded = load_credential_in(&dir, id).unwrap().unwrap();
            save_credential_in(&dir, id, &loaded).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.join(format!("{id}.yaml"))).unwrap(),
                body
            );
        }
        assert_eq!(
            ensure_access_token_in(&dir, "api", &FakeFlow("openai-codex")).unwrap(),
            "sk-051"
        );

        // The runtime registers a flow for both stored OAuth types; a refresh
        // dispatches to the flow registered under the credential's type.
        let registered =
            crate::provider_kinds::load(&hya_bundle::BuiltinBundleSelection::full()).unwrap();
        let mut kinds = hya_provider::ProviderKinds::core();
        for oauth_type in ["openai-codex", "grok-build"] {
            assert!(registered.auth_flow(oauth_type).is_some(), "{oauth_type}");
            let flow: Arc<dyn ProviderAuthFlow> = Arc::new(FakeFlow(oauth_type));
            kinds
                .register(
                    "test",
                    hya_provider::ProviderKindEntry {
                        protocol: hya_provider::test_support::stub_protocol(oauth_type),
                        auth_flow: Some(flow),
                    },
                )
                .unwrap();
        }
        for (id, access) in [("codex", "codex-access"), ("grok", "grok-access")] {
            let Some(AuthCredential::OAuth(stored)) = load_credential_in(&dir, id).unwrap() else {
                panic!("{id} is not an OAuth credential");
            };
            let flow = kinds.auth_flow(&stored.oauth_type).unwrap();
            let token = ensure_access_token_in(&dir, id, flow.as_ref()).unwrap();
            assert_eq!(
                token,
                format!("{access}-refreshed-by-{}", stored.oauth_type)
            );
            let Some(AuthCredential::OAuth(saved)) = load_credential_in(&dir, id).unwrap() else {
                panic!("{id} lost its OAuth credential");
            };
            assert_eq!(saved.access_token, token);
            // A refresh that omits account claims keeps the stored account.
            assert_eq!(saved.account_id, stored.account_id);
        }
    }
}
