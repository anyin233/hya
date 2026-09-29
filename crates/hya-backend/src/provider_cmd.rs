//! `hya provider add|list|remove`: add a model provider with a guided prompt
//! (base URL, protocol, API key), checking its model list before anything is
//! saved; list configured providers with their models; remove one.
//!
//! Declarations go to `config.yaml` (`providers.<id>`), keys to
//! `auth/<id>.yaml`, and fetched models to the model cache, the same stores
//! the TUI Provider View writes through `PUT /v1/providers/{id}`. A running
//! backend of the database is told to rebuild the provider, so it serves the
//! change without a restart.

use std::collections::BTreeMap;

use anyhow::Context as _;
use clap::{Args, Subcommand, ValueEnum};
use hya_app::{auth, config, model_cache};

use crate::prompt;

#[derive(Debug, Subcommand)]
pub(crate) enum ProviderCommand {
    /// Add (or replace) a provider: asks for anything not given as a flag,
    /// fetches the provider's model list with the key, then saves it.
    Add(AddArgs),
    /// List configured providers with their protocol, base URL, key, and models.
    List {
        /// Fetch every provider's model list now instead of showing the cache.
        #[arg(long)]
        refresh: bool,
    },
    /// Remove a provider from config.yaml with its saved key and cached models.
    Remove {
        /// Provider id (as in `hya provider list`).
        id: String,
        /// Do not ask for confirmation.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Provider id, the `<id>` of `<id>/<model>` (default: from the base URL host).
    #[arg(long)]
    name: Option<String>,
    /// API base URL, e.g. `https://api.openai.com/v1` (models are listed at `<base>/models`).
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,
    /// Wire protocol the provider speaks.
    #[arg(long, value_enum)]
    protocol: Option<Protocol>,
    /// API key (visible in process listings; omit it to be asked with hidden input).
    #[arg(long, value_name = "KEY")]
    api_key: Option<String>,
    /// Replace an existing provider of the same id without asking.
    #[arg(short = 'y', long)]
    yes: bool,
}

/// Protocols a provider can be added with, and their `config.yaml` kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Protocol {
    /// OpenAI Chat Completions (`kind: openai`).
    OpenaiChat,
    /// OpenAI Responses (`kind: openai-response`).
    OpenaiResponses,
    /// Anthropic Messages (`kind: anthropic`).
    #[value(alias = "anthropic-message")]
    AnthropicMessages,
}

impl Protocol {
    const ALL: [Self; 3] = [
        Self::OpenaiChat,
        Self::OpenaiResponses,
        Self::AnthropicMessages,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai-chat",
            Self::OpenaiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::OpenaiChat => "OpenAI Chat Completions  (<base>/chat/completions)",
            Self::OpenaiResponses => "OpenAI Responses         (<base>/responses)",
            Self::AnthropicMessages => "Anthropic Messages       (<base>/messages)",
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai",
            Self::OpenaiResponses => "openai-response",
            Self::AnthropicMessages => "anthropic",
        }
    }

    /// The protocol name for a `config.yaml` kind; other kinds (Codex, Grok
    /// Build, Google) are shown as their kind.
    fn label_for_kind(kind: &str) -> &str {
        match kind {
            "openai" | "openai-compatible" | "openai-completion" => Self::OpenaiChat.name(),
            "openai-response" => Self::OpenaiResponses.name(),
            "anthropic" => Self::AnthropicMessages.name(),
            other => other,
        }
    }
}

/// Run one `hya provider` subcommand; `db` names the backend to keep in sync.
pub(crate) async fn run(command: ProviderCommand, db: String) -> anyhow::Result<()> {
    match command {
        ProviderCommand::Add(args) => add(args, &db).await,
        ProviderCommand::List { refresh } => list(refresh).await,
        ProviderCommand::Remove { id, yes } => remove(&id, yes, &db).await,
    }
}

/// Answer `question`, or fail naming the flag that would have answered it.
fn required(question: &str, flag: &str, hidden: bool) -> anyhow::Result<String> {
    prompt::ask(question, hidden)?.with_context(|| format!("no answer on stdin; pass {flag}"))
}

/// A provider id from the base URL host: `api.openai.com` → `openai`,
/// `api.12th.day` → `12th`; `local` for an IP address or `localhost`.
fn suggested_id(base_url: &str) -> String {
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if host.is_empty()
        || host == "localhost"
        || host.parse::<std::net::IpAddr>().is_ok()
        || host.starts_with('[')
    {
        return "local".to_string();
    }
    let labels: Vec<&str> = host
        .split('.')
        .filter(|label| !matches!(*label, "api" | "www" | "gateway"))
        .collect();
    let name = labels.first().copied().unwrap_or("provider");
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if hya_server::valid_provider_id(&id) && id != "hya" {
        id
    } else {
        "provider".to_string()
    }
}

fn check_id(id: &str) -> anyhow::Result<()> {
    if !hya_server::valid_provider_id(id) || id == "hya" {
        anyhow::bail!(
            "invalid provider id `{id}`: use letters, digits, `-`, `_`, or `.` (not `hya`)"
        );
    }
    Ok(())
}

fn ask_base_url() -> anyhow::Result<String> {
    loop {
        let url = required(
            "Base URL (e.g. https://api.openai.com/v1): ",
            "--base-url",
            false,
        )?;
        match config::validate_base_url(&url) {
            Ok(()) => return Ok(url.trim_end_matches('/').to_string()),
            Err(error) => eprintln!("{error}"),
        }
    }
}

fn ask_protocol(base_url: &str) -> anyhow::Result<Protocol> {
    let guess = if base_url.contains("anthropic") {
        Protocol::AnthropicMessages
    } else {
        Protocol::OpenaiChat
    };
    eprintln!("Protocol:");
    for (index, protocol) in Protocol::ALL.iter().enumerate() {
        eprintln!(
            "  {}) {:<19} {}",
            index + 1,
            protocol.name(),
            protocol.describe()
        );
    }
    let default = Protocol::ALL.iter().position(|p| *p == guess).unwrap_or(0) + 1;
    loop {
        let answer = required(&format!("Choose [{default}]: "), "--protocol", false)?;
        if answer.is_empty() {
            return Ok(guess);
        }
        let chosen = answer
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| Protocol::ALL.get(index).copied())
            .or_else(|| Protocol::from_str(&answer, true).ok());
        match chosen {
            Some(protocol) => return Ok(protocol),
            None => eprintln!("choose 1-{} or a protocol name", Protocol::ALL.len()),
        }
    }
}

fn ask_id(base_url: &str) -> anyhow::Result<String> {
    let suggested = suggested_id(base_url);
    loop {
        let answer = required(&format!("Provider name [{suggested}]: "), "--name", false)?;
        let id = if answer.is_empty() {
            suggested.clone()
        } else {
            answer
        };
        match check_id(&id) {
            Ok(()) => return Ok(id),
            Err(error) => eprintln!("{error}"),
        }
    }
}

/// Most model ids `add` prints; `hya provider list` shows them all.
const SHOWN_MODELS: usize = 20;

/// The printed model list: the first [`SHOWN_MODELS`] ids, wrapped.
fn model_summary(ids: &[&str]) -> String {
    let mut out = String::new();
    let mut width = 0;
    for id in ids.iter().take(SHOWN_MODELS) {
        if width > 0 && width + id.len() + 2 > 78 {
            out.push('\n');
            width = 0;
        }
        if width == 0 {
            out.push_str("  ");
            width = 2;
        } else {
            out.push_str(", ");
            width += 2;
        }
        out.push_str(id);
        width += id.len();
    }
    if ids.len() > SHOWN_MODELS {
        out.push_str(&format!(
            "\n  … and {} more (`hya provider list`)",
            ids.len() - SHOWN_MODELS
        ));
    }
    out
}

/// Ask before replacing a declared provider (skipped with `--yes`).
fn confirm_replace(id: &str, yes: bool) -> anyhow::Result<()> {
    let Some(existing) = config::provider_declarations()?
        .into_iter()
        .find(|provider| provider.id == id)
    else {
        return Ok(());
    };
    if yes
        || prompt::confirm(&format!(
            "Provider `{id}` exists ({}, {}). Replace it?",
            Protocol::label_for_kind(&existing.kind),
            existing.base_url
        ))?
    {
        return Ok(());
    }
    anyhow::bail!("provider `{id}` left unchanged")
}

async fn add(args: AddArgs, db: &str) -> anyhow::Result<()> {
    // A given id is checked first, so a refused replacement asks nothing else.
    let named = match &args.name {
        Some(name) => {
            check_id(name)?;
            confirm_replace(name, args.yes)?;
            Some(name.clone())
        }
        None => None,
    };
    let base_url = match args.base_url {
        Some(url) => {
            config::validate_base_url(&url).map_err(anyhow::Error::msg)?;
            url.trim_end_matches('/').to_string()
        }
        None => ask_base_url()?,
    };
    let protocol = match args.protocol {
        Some(protocol) => protocol,
        None => ask_protocol(&base_url)?,
    };
    let api_key = match args.api_key {
        Some(key) => key,
        None => required(
            "API key (input hidden; empty for none): ",
            "--api-key",
            true,
        )?,
    };
    let api_key = Some(api_key.trim().to_string()).filter(|key| !key.is_empty());

    let probe_id = named.clone().unwrap_or_else(|| suggested_id(&base_url));
    eprintln!("Fetching models from {base_url}/models …");
    let discovery =
        config::probe_provider(&probe_id, protocol.kind(), &base_url, api_key.as_deref())
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
    let ids = discovery.model_ids();
    if discovery.ok() {
        println!(
            "Found {} model{}:",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        );
        if !ids.is_empty() {
            println!("{}", model_summary(&ids));
        }
    } else {
        let reason = match discovery.label() {
            "auth_rejected" => "the endpoint rejected the API key".to_string(),
            "auth_required" => "the endpoint requires an API key".to_string(),
            label => label.replace('_', " "),
        };
        let detail = discovery
            .error_message()
            .map(|message| format!(" ({message})"))
            .unwrap_or_default();
        eprintln!("Could not list models: {reason}{detail}.");
        if !prompt::confirm("Save the provider anyway?")? {
            anyhow::bail!("provider not saved: {reason}");
        }
    }

    let id = match named {
        Some(id) => id,
        None => {
            let id = ask_id(&base_url)?;
            confirm_replace(&id, args.yes)?;
            id
        }
    };

    let config_path = config::active_config_path();
    config::upsert_provider_entry(&config_path, &id, protocol.kind(), &base_url)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    match &api_key {
        Some(key) => {
            auth::save_token(&id, key).with_context(|| format!("save the key of `{id}`"))?
        }
        // A replaced provider without a key must not keep the old endpoint's key.
        None => {
            auth::remove_token(&id).with_context(|| format!("remove the old key of `{id}`"))?;
        }
    }
    config::store_discovery(&id, &discovery).await;

    println!(
        "Saved provider `{id}` ({}, {base_url}) to {}",
        protocol.name(),
        config_path.display()
    );
    if api_key.is_some()
        && let Some(dir) = auth::auth_dir()
    {
        println!(
            "Saved its key to {}",
            dir.join(format!("{id}.yaml")).display()
        );
    }
    if let Some(first) = ids.first() {
        println!(
            "Use a model with `hya --model {id}/{first}`, or set `default_model: {id}/{first}` in config.yaml"
        );
    }
    sync_running_backend(db, &id, SyncAction::Rebuild).await;
    Ok(())
}

async fn list(refresh: bool) -> anyhow::Result<()> {
    let declared = config::provider_declarations()?;
    if declared.is_empty() {
        println!("No providers configured. Add one with `hya provider add`.");
        return Ok(());
    }
    let resolved = config::load().await?.context("config.yaml is missing")?;
    let catalog = if refresh {
        config::rebuild_providers(
            None,
            config::DiscoverMode::Always,
            resolved.catalog.as_ref(),
            &resolved.router,
        )
        .await?
        .catalog
    } else {
        resolved.catalog
    };
    let mut models: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for model in catalog.models() {
        models
            .entry(model.provider_id.as_str())
            .or_default()
            .push(model.model_id.as_str());
    }
    for (index, provider) in declared.iter().enumerate() {
        if index > 0 {
            println!();
        }
        let key = match auth::load_credential(&provider.id) {
            Some(credential) if credential.oauth().is_some() => "oauth",
            Some(_) => "saved key",
            None if provider.inline_api_key => "config api_key",
            None => "no key",
        };
        println!(
            "{}  {}  {}  {key}",
            provider.id,
            Protocol::label_for_kind(&provider.kind),
            provider.base_url
        );
        match models.get_mut(provider.id.as_str()) {
            Some(ids) => {
                ids.sort_unstable();
                for id in ids.iter() {
                    println!("  {}/{id}", provider.id);
                }
            }
            None => println!("  (no models known; `hya provider list --refresh` fetches them)"),
        }
    }
    Ok(())
}

async fn remove(id: &str, yes: bool, db: &str) -> anyhow::Result<()> {
    let Some(provider) = config::provider_declarations()?
        .into_iter()
        .find(|provider| provider.id == id)
    else {
        anyhow::bail!("provider `{id}` is not configured (see `hya provider list`)");
    };
    if !yes
        && !prompt::confirm(&format!(
            "Remove provider `{id}` ({}, {}) with its saved key and cached models?",
            Protocol::label_for_kind(&provider.kind),
            provider.base_url
        ))?
    {
        anyhow::bail!("provider `{id}` left unchanged");
    }
    config::remove_provider_entry(&config::active_config_path(), id)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    auth::remove_token(id).with_context(|| format!("remove the key of `{id}`"))?;
    model_cache::store_provider_or_warn(id, &[]).await;
    println!("Removed provider `{id}`");
    for path in config::provider_model_references(id)? {
        println!("warning: `{path}` in config.yaml still names a model of `{id}`");
    }
    sync_running_backend(db, id, SyncAction::Drop).await;
    Ok(())
}

enum SyncAction {
    /// Re-read the provider from config, credential, and cache (fetching its models).
    Rebuild,
    /// Drop the removed provider's route and models.
    Drop,
}

/// Bring the database's running backend (if any) up to date without a
/// restart. Best effort: a failure is reported, never fatal, because the
/// files are already written and a restart applies them.
async fn sync_running_backend(db: &str, id: &str, action: SyncAction) {
    let Some(found) = crate::daemon::running(db).await else {
        return;
    };
    let url = found.url.trim_end_matches('/');
    let client = reqwest::Client::new();
    let request = match action {
        SyncAction::Rebuild => client
            .post(format!("{url}/v1/providers/{id}/refresh"))
            .json(&serde_json::json!({})),
        // Removing the (already deleted) key rebuilds the provider, and a
        // provider no longer in config.yaml loses its route and models.
        SyncAction::Drop => client.delete(format!("{url}/v1/auth/{id}")),
    };
    match request
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            println!("Updated the running backend at {url}");
        }
        Ok(response) => eprintln!(
            "warning: the running backend at {url} answered {} for `{id}`; `hya serve restart` applies the change",
            response.status()
        ),
        Err(error) => eprintln!(
            "warning: could not reach the running backend at {url} ({error}); `hya serve restart` applies the change"
        ),
    }
}
