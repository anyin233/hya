use anyhow::Context as _;
use clap::Subcommand;

use crate::auth;

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
