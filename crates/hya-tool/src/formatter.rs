//! External code-formatter plane used after write/edit/patch.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;

/// How the formatter plane chooses which formatters to run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FormatterConfig {
    /// No formatters run.
    #[default]
    Disabled,
    /// Built-in language formatters only.
    Builtins,
    /// Merge custom entries over the builtin set.
    Custom(BTreeMap<String, FormatterEntry>),
}

/// One named formatter override in a custom config map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormatterEntry {
    /// When true, remove this formatter from the active set.
    pub disabled: bool,
    /// Explicit argv; when set, runs instead of the builtin probe.
    pub command: Option<Vec<String>>,
    /// Extra environment variables for the command.
    pub environment: BTreeMap<String, String>,
    /// File extensions this formatter owns (with leading dots).
    pub extensions: Option<Vec<String>>,
}

/// Status row for one configured formatter (name, extensions, enabled).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FormatterStatus {
    name: String,
    extensions: Vec<String>,
    enabled: bool,
}

impl FormatterStatus {
    /// Build a status row for UI / diagnostics.
    #[must_use]
    pub fn new(name: impl Into<String>, extensions: Vec<String>, enabled: bool) -> Self {
        Self {
            name: name.into(),
            extensions,
            enabled,
        }
    }
}

/// Formatter provider failure.
#[derive(Error, Debug)]
#[error("{0}")]
pub struct FormatterError(pub String);

/// Backend that can list formatters and format a file in place.
#[async_trait]
pub trait FormatterProvider: Send + Sync {
    /// List configured formatters and whether each is currently runnable.
    async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError>;
    /// Format `file` under `workdir`; returns whether any formatter ran.
    async fn format_file(&self, _workdir: &Path, _file: &Path) -> Result<bool, FormatterError> {
        Ok(false)
    }
}

/// Optional formatter provider used by mutating file tools.
#[derive(Clone, Default)]
pub struct FormatterPlane {
    provider: Option<Arc<dyn FormatterProvider>>,
}

impl FormatterPlane {
    /// Wrap a concrete provider.
    #[must_use]
    pub fn new(provider: Arc<dyn FormatterProvider>) -> Self {
        Self {
            provider: Some(provider),
        }
    }

    /// Whether a concrete formatter provider is attached.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.provider.is_some()
    }

    /// Provider status rows, or empty when disconnected.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn status(&self, workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        match &self.provider {
            Some(provider) => provider.status(workdir).await,
            None => Ok(Vec::new()),
        }
    }

    /// Format a file; returns `false` when no provider is configured.
    ///
    /// # Errors
    /// Propagates provider failures.
    pub async fn format_file(&self, workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        match &self.provider {
            Some(provider) => provider.format_file(workdir, file).await,
            None => Ok(false),
        }
    }
}
