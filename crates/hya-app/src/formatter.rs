//! External command formatter provider configured by the application host.

use std::path::Path;
use std::process::Stdio;

use async_trait::async_trait;
use hya_tool::{FormatterConfig, FormatterError, FormatterProvider, FormatterStatus};
use tokio::process::Command;

use crate::formatter_command::{FormatterCommand, builtin_enabled, command_for_definition};
use crate::formatter_definition::definitions_for_config;

/// Default provider that runs builtin/custom command formatters from config.
pub struct BuiltinFormatterProvider {
    config: FormatterConfig,
}

impl BuiltinFormatterProvider {
    /// Create a provider for the given config.
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
                run_formatter(command, workdir, file).await?;
                ran = true;
            }
        }
        Ok(ran)
    }
}

fn dotted_extension(file: &Path) -> Option<String> {
    file.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(|extension| format!(".{extension}"))
}

async fn run_formatter(
    command: FormatterCommand,
    workdir: &Path,
    file: &Path,
) -> Result<(), FormatterError> {
    let file = file.to_string_lossy();
    let argv: Vec<String> = command
        .argv
        .into_iter()
        .map(|part| part.replace("$FILE", &file))
        .collect();
    let Some((program, args)) = argv.split_first() else {
        return Ok(());
    };
    let program_path = Path::new(program);
    let resolved_program;
    let program = if program_path.is_relative() && program.contains('/') {
        resolved_program = workdir.join(program_path);
        resolved_program.as_path()
    } else {
        program_path
    };
    let status = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .envs(command.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| {
            FormatterError(format!(
                "formatter spawn failed: {}: {error}",
                program.display()
            ))
        })?;
    if !status.success() {
        return Err(FormatterError(format!(
            "formatter exited with {status}: {}",
            program.display()
        )));
    }
    Ok(())
}
