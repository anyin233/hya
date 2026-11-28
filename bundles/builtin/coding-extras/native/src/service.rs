//! Long-lived [`hya_runtime_abi::NativeService`] managing LSP servers and
//! code formatters for `hya/coding-extras`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use hya_runtime_abi::{HostError, HostRuntime, NativeService, ServiceHealth};
use hya_tool::{FormatterPlane, LspPlane};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::formatter::BuiltinFormatterProvider;
use crate::formatter::config::RawFormatterConfig;
use crate::lsp_service::{self, ProcessLspProvider};

/// Bundle identity owning the coding-extras service and hooks.
pub const BUNDLE_ID: &str = "hya/coding-extras";

/// Stable [`NativeService::id`] for the LSP and formatter background service.
pub const SERVICE_ID: &str = "lsp-formatter";

/// Parsed `config.yml` shape for `hya/coding-extras`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CodingExtrasConfig {
    /// Formatter configuration (`false`, `true`, or named override map).
    #[serde(default)]
    pub formatter: RawFormatterConfig,
    /// LSP configuration (`false`, `true`, or named server map).
    #[serde(default)]
    pub lsp: Option<lsp_service::config::Selection>,
}

/// Shared runtime state holding the active [`FormatterPlane`] and [`LspPlane`]
/// configured by [`CodingExtrasService`].
#[derive(Default)]
pub struct CodingExtrasState {
    formatter: RwLock<FormatterPlane>,
    lsp: RwLock<LspPlane>,
    lsp_provider: RwLock<Option<Arc<ProcessLspProvider>>>,
    started: AtomicBool,
}

impl CodingExtrasState {
    /// Create an empty, unstarted state container.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a pre-populated state container (for testing or direct embedding).
    #[must_use]
    pub fn with_planes(formatter: FormatterPlane, lsp: LspPlane) -> Self {
        Self {
            formatter: RwLock::new(formatter),
            lsp: RwLock::new(lsp),
            lsp_provider: RwLock::new(None),
            started: AtomicBool::new(true),
        }
    }

    /// Current active formatter plane.
    #[must_use]
    pub fn formatter(&self) -> FormatterPlane {
        self.formatter
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Current active LSP plane.
    #[must_use]
    pub fn lsp(&self) -> LspPlane {
        self.lsp
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether [`CodingExtrasService::start`] has succeeded and not yet stopped.
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Configure formatter and LSP planes from a decoded bundle configuration JSON value.
    ///
    /// # Errors
    /// Returns [`HostError::InvalidInput`] if the configuration cannot be parsed
    /// or declares an invalid LSP server entry.
    pub fn configure_from_value(&self, value: &Value) -> Result<(), HostError> {
        let config: CodingExtrasConfig = if value.is_null() {
            CodingExtrasConfig::default()
        } else {
            serde_json::from_value(value.clone()).map_err(|error| {
                HostError::InvalidInput(format!("invalid {BUNDLE_ID} bundle config: {error}"))
            })?
        };
        let formatter_plane = FormatterPlane::new(Arc::new(BuiltinFormatterProvider::new(
            config.formatter.into(),
        )));
        let (lsp_plane, lsp_provider) =
            lsp_service::from_selection(config.lsp).map_err(HostError::InvalidInput)?;

        *self
            .formatter
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = formatter_plane;
        *self
            .lsp
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = lsp_plane;
        *self
            .lsp_provider
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = lsp_provider;
        self.started.store(true, Ordering::Release);
        Ok(())
    }

    /// Shut down any running LSP server subprocesses and reset planes.
    pub async fn shutdown(&self) {
        self.started.store(false, Ordering::Release);
        let provider = self
            .lsp_provider
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        *self
            .lsp
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = LspPlane::default();
        *self
            .formatter
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = FormatterPlane::default();
        if let Some(provider) = provider {
            provider.shutdown_all().await;
        }
    }
}

/// Long-lived [`NativeService`] owning the LSP and formatter lifecycle for
/// `hya/coding-extras`.
pub struct CodingExtrasService {
    state: Arc<CodingExtrasState>,
}

impl CodingExtrasService {
    /// Wrap shared [`CodingExtrasState`] in a managed [`NativeService`].
    #[must_use]
    pub fn new(state: Arc<CodingExtrasState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl NativeService for CodingExtrasService {
    fn id(&self) -> &str {
        SERVICE_ID
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        let config_value = match runtime.catalog_reader().bundle_config(BUNDLE_ID).await {
            Ok(value) => value,
            Err(HostError::NotFound(_) | HostError::Unavailable(_)) => Value::Null,
            Err(error) => return Err(error),
        };
        self.state.configure_from_value(&config_value)
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.state.shutdown().await;
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        if !self.state.is_started() {
            return ServiceHealth::unhealthy("stopped");
        }
        let mut health = ServiceHealth::healthy("ready");
        health.details = Some(json!({
            "lsp_configured": self.state.lsp().is_configured(),
            "formatter_configured": self.state.formatter().is_configured(),
        }));
        health
    }
}
