use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use thiserror::Error;
use tokio::process::Command;

use crate::formatter_command::{FormatterCommand, builtin_enabled, command_for_definition};
use crate::formatter_definition::definitions_for_config;

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

    async fn format_file(&self, workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        let Some(extension) = dotted_extension(file) else {
            return Ok(false);
        };
        let mut ran = false;
        for item in definitions_for_config(&self.config) {
            if !item
                .extensions
                .iter()
                .any(|candidate| candidate == &extension)
            {
                continue;
            }
            if let Some(command) = command_for_definition(&item, workdir) {
                ran = true;
                run_formatter(command, workdir, file).await;
            }
        }
        Ok(ran)
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

fn dotted_extension(file: &Path) -> Option<String> {
    file.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(|extension| format!(".{extension}"))
}

async fn run_formatter(command: FormatterCommand, workdir: &Path, file: &Path) {
    let file = file.to_string_lossy();
    let argv: Vec<String> = command
        .argv
        .into_iter()
        .map(|part| part.replace("$FILE", &file))
        .collect();
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let _status = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .envs(command.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}
