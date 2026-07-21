use anyhow::Context as _;

use crate::config::ModelEntry;

pub(crate) fn cmd_models(
    models: Vec<ModelEntry>,
    fallback_model: &str,
    provider: Option<String>,
    verbose: bool,
    _refresh: bool,
) -> anyhow::Result<()> {
    let lines = model_lines(&models, provider.as_deref(), fallback_model)
        .map_err(anyhow::Error::msg)
        .context("list models")?;
    for line in lines {
        println!("{line}");
        if verbose {
            let (provider, id) = line.split_once('/').unwrap_or(("hya", line.as_str()));
            println!(
                "{}",
                serde_json::json!({
                    "id": id,
                    "provider": provider,
                })
            );
        }
    }
    Ok(())
}

fn model_lines(
    models: &[ModelEntry],
    provider: Option<&str>,
    fallback_model: &str,
) -> Result<Vec<String>, String> {
    let mut lines = if models.is_empty() {
        if provider.is_none_or(|provider| provider == "hya") {
            vec![format!("hya/{fallback_model}")]
        } else {
            Vec::new()
        }
    } else {
        models
            .iter()
            .filter(|model| provider.is_none_or(|provider| model.provider == provider))
            .map(|model| format!("{}/{}", model.provider, model.id))
            .collect::<Vec<_>>()
    };
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

    #[test]
    fn model_lines_list_provider_model_ids() {
        let models = vec![
            ModelEntry {
                provider: "openai".to_string(),
                id: "gpt-5.5".to_string(),
                reasoning_variants: Vec::new(),
                reasoning_default: None,
            },
            ModelEntry {
                provider: "anthropic".to_string(),
                id: "claude-sonnet-4-6".to_string(),
                reasoning_variants: Vec::new(),
                reasoning_default: None,
            },
        ];

        assert_eq!(
            super::model_lines(&models, None, "fake"),
            Ok(vec![
                "anthropic/claude-sonnet-4-6".to_string(),
                "openai/gpt-5.5".to_string(),
            ])
        );
        assert_eq!(
            super::model_lines(&models, Some("openai"), "fake"),
            Ok(vec!["openai/gpt-5.5".to_string()])
        );
    }
}
