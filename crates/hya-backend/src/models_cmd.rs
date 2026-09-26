use anyhow::Context as _;
use hya_provider::{ProviderCatalogSnapshot, ProviderModel};

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
            let row = catalog
                .models()
                .iter()
                .find(|model| model.provider_id == provider && model.model_id == id);
            println!("{}", verbose_line(provider, id, row));
        }
    }
    Ok(())
}

/// One `--verbose` JSON line: id, provider, source (`remote`, `config`,
/// `override`, or `offline`), and the known metadata.
fn verbose_line(provider: &str, id: &str, row: Option<&ProviderModel>) -> serde_json::Value {
    let mut line = serde_json::json!({
        "id": id,
        "provider": provider,
        "source": row.map_or("unknown", |model| model.source.as_str()),
    });
    if let Some(model) = row {
        if let Some(name) = model.display_name.as_deref() {
            line["name"] = serde_json::json!(name);
        }
        line["context"] = serde_json::json!(model.capabilities.max_context);
        if model.capabilities.max_output > 0 {
            line["output"] = serde_json::json!(model.capabilities.max_output);
        }
        line["reasoning"] = serde_json::json!(!model.reasoning_variants.is_empty());
    }
    line
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

#[cfg(test)]
mod tests {
    use super::*;
    use hya_provider::ModelCatalogSource;

    fn model(provider: &str, id: &str, source: ModelCatalogSource) -> ProviderModel {
        ProviderModel {
            provider_id: provider.to_string(),
            model_id: id.to_string(),
            capabilities: Default::default(),
            reasoning_variants: Vec::new(),
            reasoning_default: None,
            display_name: None,
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
    fn verbose_line_names_the_source_and_metadata() {
        let mut row = model("gw", "vendor/m", ModelCatalogSource::Overridden);
        row.display_name = Some("Vendor M".to_string());
        row.capabilities.max_context = 64_000;
        row.capabilities.max_output = 4_096;
        assert_eq!(
            super::verbose_line("gw", "vendor/m", Some(&row)),
            serde_json::json!({
                "id": "vendor/m",
                "provider": "gw",
                "source": "override",
                "name": "Vendor M",
                "context": 64000,
                "output": 4096,
                "reasoning": false,
            })
        );
        let remote = model("gw", "r", ModelCatalogSource::Discovered);
        assert_eq!(
            super::verbose_line("gw", "r", Some(&remote))["source"],
            "remote"
        );
        let config = model("gw", "c", ModelCatalogSource::Configured);
        assert_eq!(
            super::verbose_line("gw", "c", Some(&config))["source"],
            "config"
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
