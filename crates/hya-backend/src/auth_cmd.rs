use anyhow::Context as _;
use clap::Subcommand;

use crate::auth;
use hya_app::auth::OAuthType;
use hya_app::oauth::{self, OAuthLoginOptions};

#[derive(Subcommand)]
pub(crate) enum AuthCommand {
    /// List provider ids with saved auth tokens.
    List,
    /// Remove a saved provider auth token.
    Logout {
        /// Provider id as it appears in your hya config.
        provider: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum OauthCommand {
    /// Run an interactive OAuth login for openai-codex or grok-build.
    Login {
        /// Provider id to write under `providers.<id>` and `auth/<id>.yaml`.
        #[arg(long)]
        provider: String,
        /// OAuth provider type: `openai-codex` or `grok-build`.
        #[arg(long = "type", value_name = "TYPE")]
        oauth_type: String,
        /// Use the device-code flow (default for openai-codex and grok-build).
        #[arg(long)]
        device: bool,
        /// openai-codex only: use localhost PKCE callback instead of Codex device-code.
        #[arg(long)]
        loopback: bool,
        /// Print the verification URL without opening a browser
        /// (default for openai-codex device login, matching Codex CLI).
        #[arg(long)]
        no_browser: bool,
        /// Open a system browser for the verification / authorize URL.
        #[arg(long)]
        browser: bool,
        /// Model id to register on the provider (default depends on type).
        #[arg(long)]
        model: Option<String>,
        /// Override the inference base URL (defaults depend on type).
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Show saved auth status (OAuth type and expiry; no secrets).
    Status {
        /// Optional provider id filter.
        provider: Option<String>,
    },
}

pub(crate) async fn login(provider: String, token: String) -> anyhow::Result<()> {
    auth::save_token(&provider, &token).with_context(|| format!("save token for {provider}"))?;
    println!("Saved auth token for provider '{provider}'.");
    Ok(())
}

pub(crate) async fn run(command: AuthCommand) -> anyhow::Result<()> {
    match command {
        AuthCommand::List => {
            let providers = auth::list_tokens().context("list auth tokens")?;
            if providers.is_empty() {
                println!("no auth tokens saved");
                return Ok(());
            }
            for provider in providers {
                println!("{provider}");
            }
            Ok(())
        }
        AuthCommand::Logout { provider } => {
            let removed = auth::remove_token(&provider)
                .with_context(|| format!("remove token for {provider}"))?;
            if removed {
                println!("Removed auth token for provider '{provider}'.");
            } else {
                println!("No auth token saved for provider '{provider}'.");
            }
            Ok(())
        }
    }
}

pub(crate) async fn run_oauth(command: OauthCommand) -> anyhow::Result<()> {
    match command {
        OauthCommand::Login {
            provider,
            oauth_type,
            device,
            loopback,
            no_browser,
            browser,
            model,
            base_url,
        } => {
            let oauth_type = OAuthType::parse(&oauth_type).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown OAuth type '{oauth_type}'; supported: openai-codex, grok-build"
                )
            })?;
            if loopback && !matches!(oauth_type, OAuthType::OpenaiCodex) {
                anyhow::bail!("--loopback is only supported for --type openai-codex");
            }
            if browser && no_browser {
                anyhow::bail!("pass only one of --browser or --no-browser");
            }
            // openai-codex defaults to Codex device-code (no local callback).
            // grok-build is always device-code. --loopback opts into localhost PKCE for codex.
            let use_loopback = loopback && matches!(oauth_type, OAuthType::OpenaiCodex);
            let device = !use_loopback
                && (device || matches!(oauth_type, OAuthType::GrokBuild | OAuthType::OpenaiCodex));
            // Codex default: print URL/code only (no auto-open). --browser enables open.
            // Other types open unless --no-browser.
            let no_browser = if browser {
                false
            } else if no_browser {
                true
            } else {
                matches!(oauth_type, OAuthType::OpenaiCodex) && !use_loopback
            };
            let result = oauth::login(OAuthLoginOptions {
                provider,
                oauth_type,
                device,
                loopback: use_loopback,
                no_browser,
                model,
                base_url,
                auth_dir: None,
                config_path: None,
                timeout: std::time::Duration::from_secs(600),
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "Saved OAuth credentials for provider '{}' ({}).",
                result.provider, result.oauth_type
            );
            println!("  auth:   {}", result.auth_path.display());
            println!(
                "  config: {} (kind: {}, base_url: {})",
                result.config_path.display(),
                result.oauth_type.provider_kind(),
                result.base_url
            );
            if result.models_from_catalog {
                println!(
                    "  models: {} from live catalog (default {}/{})",
                    result.models.len(),
                    result.provider,
                    result.model
                );
                for id in &result.models {
                    println!("    - {id}");
                }
            } else {
                println!(
                    "  model:  {}/{} (catalog unavailable; only default written)",
                    result.provider, result.model
                );
            }
            Ok(())
        }
        OauthCommand::Status { provider } => {
            let dir = auth::auth_dir().ok_or_else(|| {
                anyhow::anyhow!("no config directory (set HOME or XDG_CONFIG_HOME)")
            })?;
            let statuses = oauth::oauth_status_in(&dir, provider.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if statuses.is_empty() {
                println!("no auth credentials saved");
                return Ok(());
            }
            for status in statuses {
                match status.oauth_type {
                    Some(oauth_type) => {
                        let state = if status.expired { "EXPIRED" } else { "ok" };
                        let exp = status.expires_at.as_deref().unwrap_or("?");
                        print!(
                            "{}  oauth={}  expires={}  status={state}",
                            status.provider, oauth_type, exp
                        );
                        if let Some(account) = status.account_id.as_deref() {
                            print!("  account={account}");
                        }
                        println!();
                        if status.expired {
                            println!(
                                "  re-login: hya-backend oauth login --provider {} --type {}",
                                status.provider, oauth_type
                            );
                        }
                    }
                    None => {
                        println!("{}  kind={}", status.provider, status.kind);
                    }
                }
            }
            Ok(())
        }
    }
}
