use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

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
}
