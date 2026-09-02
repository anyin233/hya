use anyhow::Context as _;
use hya_provider::{ModelCatalogSource, ProviderCatalogSnapshot, ProviderModel};

pub(crate) fn cmd_models(
    catalog: &ProviderCatalogSnapshot,
    provider: Option<String>,
    verbose: bool,
) -> anyhow::Result<()> {
    let lines = model_lines(catalog.models(), provider.as_deref())
        .map_err(anyhow::Error::msg)
        .context("list models")?;
    for line in lines {
        println!("{line}");
        if verbose {
            let (provider, id) = line.split_once('/').unwrap_or(("hya", line.as_str()));
            let source = catalog
                .models()
                .iter()
                .find(|model| model.provider_id == provider && model.model_id == id)
                .map(|model| source_name(model.source))
                .unwrap_or("unknown");
            println!(
                "{}",
                serde_json::json!({
                    "id": id,
                    "provider": provider,
                    "source": source,
                })
            );
        }
    }
    Ok(())
}

fn model_lines(models: &[ProviderModel], provider: Option<&str>) -> Result<Vec<String>, String> {
    let mut lines = models
        .iter()
        .filter(|model| provider.is_none_or(|provider| model.provider_id == provider))
        .map(|model| format!("{}/{}", model.provider_id, model.model_id))
        .collect::<Vec<_>>();
    lines.sort();
    if lines.is_empty() {
        return Err(format!(
            "Provider not found: {}",
            provider.unwrap_or_default()
        ));
    }
    Ok(lines)
}

fn source_name(source: ModelCatalogSource) -> &'static str {
    match source {
        ModelCatalogSource::Configured => "configured",
        ModelCatalogSource::Discovered => "discovered",
        ModelCatalogSource::Offline => "offline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str, source: ModelCatalogSource) -> ProviderModel {
        ProviderModel {
            provider_id: provider.to_string(),
            model_id: id.to_string(),
            capabilities: Default::default(),
            reasoning_variants: Vec::new(),
            reasoning_default: None,
            source,
        }
    }

    #[test]
    fn model_lines_list_provider_model_ids() {
        let models = vec![
            model("openai", "gpt-5.5", ModelCatalogSource::Configured),
            model(
                "anthropic",
                "claude-sonnet-4-6",
                ModelCatalogSource::Discovered,
            ),
        ];

        assert_eq!(
            super::model_lines(&models, None),
            Ok(vec![
                "anthropic/claude-sonnet-4-6".to_string(),
                "openai/gpt-5.5".to_string(),
            ])
        );
        assert_eq!(
            super::model_lines(&models, Some("openai")),
            Ok(vec!["openai/gpt-5.5".to_string()])
        );
    }

    #[test]
    fn model_lines_rejects_missing_provider_without_fallback_row() {
        let models = vec![model("openai", "gpt-5.5", ModelCatalogSource::Configured)];
        assert_eq!(
            super::model_lines(&models, Some("missing")),
            Err("Provider not found: missing".to_string())
        );
    }
}
