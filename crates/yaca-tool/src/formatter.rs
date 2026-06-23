use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

use crate::formatter_definition::{builtin_enabled, definitions_for_config};

pub use crate::formatter_definition::{FormatterConfig, FormatterEntry};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatterStatus {
    name: String,
    extensions: Vec<String>,
    enabled: bool,
}

impl FormatterStatus {
    #[must_use]
    pub fn new(name: impl Into<String>, extensions: Vec<String>, enabled: bool) -> Self {
        Self {
            name: name.into(),
            extensions,
            enabled,
        }
    }
}

#[derive(Error, Debug)]
#[error("{0}")]
pub struct FormatterError(pub String);

#[async_trait]
pub trait FormatterProvider: Send + Sync {
    async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError>;
    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Ok(false)
    }
}

pub struct BuiltinFormatterProvider {
    config: FormatterConfig,
}

impl BuiltinFormatterProvider {
    #[must_use]
    pub fn new(config: FormatterConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl FormatterProvider for BuiltinFormatterProvider {
    async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        let definitions = definitions_for_config(&self.config);
        Ok(definitions
            .into_iter()
            .map(|item| {
                let enabled = item.command.is_some()
                    || item
                        .check
                        .is_some_and(|check| builtin_enabled(check, workdir));
                FormatterStatus::new(item.name, item.extensions, enabled)
            })
            .collect())
    }

    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Ok(false)
    }
}

#[derive(Clone, Default)]
pub struct FormatterPlane {
    provider: Option<Arc<dyn FormatterProvider>>,
}

impl FormatterPlane {
    #[must_use]
    pub fn new(provider: Arc<dyn FormatterProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    pub async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        match &self.provider {
            Some(provider) => provider.status(workdir).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn format_file(&self, workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        match &self.provider {
            Some(provider) => provider.format_file(workdir, file).await,
            None => Ok(false),
        }
    }
}
