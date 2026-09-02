//! Immutable model catalog snapshots shared by runtime and presentation layers.

use std::collections::HashSet;
use std::sync::Arc;

use hya_proto::ModelRef;

use crate::{ProviderKind, ProviderModel};

/// Origin of a model row in the startup catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCatalogSource {
    /// Authored in Hya's provider configuration.
    Configured,
    /// Returned by a declared provider's startup catalog endpoint.
    Discovered,
    /// The Hya-owned local echo row used when no live row exists.
    Offline,
}

/// Origin of a declared provider's model resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCatalogSource {
    /// The provider contributed explicitly configured rows.
    Configured,
    /// The provider contributed rows from startup discovery.
    Discovered,
    /// The provider declaration contributed no model rows.
    None,
    /// The local Hya offline provider.
    Offline,
}

/// Non-secret authentication state recorded for a provider at startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAuthState {
    /// Hya resolved a credential for this provider.
    Credentialed,
    /// No Hya credential was available and no credential was sent.
    Unauthenticated,
    /// An unauthenticated discovery request received 401/403.
    AuthRequired,
    /// A credentialed discovery request received 401/403.
    AuthRejected,
    /// Authentication does not apply to the built-in local provider.
    NotApplicable,
}

/// Non-secret result of resolving one declared provider's startup catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCatalogResult {
    /// One or more model rows were resolved.
    Models,
    /// The provider returned a valid empty catalog.
    Empty,
    /// The provider endpoint was unavailable or failed before producing rows.
    Unavailable,
    /// The provider endpoint returned an invalid response.
    Invalid,
    /// Hya has no safe startup adapter for this provider kind.
    Unsupported,
    /// The built-in local provider supplies the canonical offline row.
    Offline,
}

/// Safe startup status for one provider declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCatalogState {
    /// Hya provider id.
    pub provider_id: String,
    /// Configured protocol kind.
    pub kind: ProviderKind,
    /// How the provider's model set was obtained.
    pub source: ProviderCatalogSource,
    /// Whether the startup request had usable Hya authentication.
    pub auth: ProviderAuthState,
    /// Non-secret resolution result.
    pub result: ProviderCatalogResult,
}

/// Additional machine-readable metadata for the local offline row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogNotice {
    /// Tell the user that a live provider must be configured.
    ConfigureProvider {
        /// Bounded, non-secret user-facing notice.
        message: String,
    },
}

impl CatalogNotice {
    /// The canonical notice shown for the offline provider.
    #[must_use]
    pub fn configure_provider() -> Self {
        Self::ConfigureProvider {
            message: "No live provider is available. Configure a provider to continue.".to_string(),
        }
    }

    /// Borrow the notice text without allocating.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::ConfigureProvider { message } => message,
        }
    }
}

/// One immutable catalog and provider-status snapshot for a process startup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCatalogSnapshot {
    models: Arc<[ProviderModel]>,
    providers: Arc<[ProviderCatalogState]>,
    default_model: ModelRef,
    notice: Option<CatalogNotice>,
}

impl ProviderCatalogSnapshot {
    /// Build a normalized, deterministic snapshot from staged provider results.
    ///
    /// Exact duplicate model references retain their first row. Rows are sorted
    /// by provider id and then model id after deduplication. When `models` is
    /// empty, this function inserts exactly the canonical `hya/offline` row and
    /// local provider status.
    #[must_use]
    pub fn build(
        models: impl IntoIterator<Item = ProviderModel>,
        providers: impl IntoIterator<Item = ProviderCatalogState>,
        requested_default: Option<ModelRef>,
    ) -> Self {
        let mut seen = HashSet::new();
        let mut models: Vec<_> = models
            .into_iter()
            .filter(|model| {
                model.provider_id != "hya"
                    || model.model_id != "offline"
                    || model.source == ModelCatalogSource::Offline
            })
            .filter(|model| seen.insert((model.provider_id.clone(), model.model_id.clone())))
            .collect();
        let mut notice = None;
        let live = models
            .iter()
            .any(|model| model.source != ModelCatalogSource::Offline);
        if live {
            models.retain(|model| model.source != ModelCatalogSource::Offline);
        } else {
            models.clear();
            models.push(ProviderModel {
                provider_id: "hya".to_string(),
                model_id: "offline".to_string(),
                capabilities: crate::Capabilities::default(),
                reasoning_variants: Vec::new(),
                reasoning_default: None,
                source: ModelCatalogSource::Offline,
            });
            notice = Some(CatalogNotice::configure_provider());
        }
        models.sort_by(|left, right| {
            left.provider_id
                .cmp(&right.provider_id)
                .then(left.model_id.cmp(&right.model_id))
        });

        let mut providers: Vec<_> = providers.into_iter().collect();
        if !live {
            providers.retain(|provider| provider.provider_id != "hya");
            providers.push(ProviderCatalogState {
                provider_id: "hya".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                source: ProviderCatalogSource::Offline,
                auth: ProviderAuthState::NotApplicable,
                result: ProviderCatalogResult::Offline,
            });
        } else {
            providers.retain(|provider| provider.source != ProviderCatalogSource::Offline);
            let mut provider_ids = providers
                .iter()
                .map(|provider| provider.provider_id.clone())
                .collect::<HashSet<_>>();
            for model in &models {
                if provider_ids.insert(model.provider_id.clone()) {
                    providers.push(ProviderCatalogState {
                        provider_id: model.provider_id.clone(),
                        kind: ProviderKind::OpenAiCompatible,
                        source: match model.source {
                            ModelCatalogSource::Configured => ProviderCatalogSource::Configured,
                            ModelCatalogSource::Discovered => ProviderCatalogSource::Discovered,
                            ModelCatalogSource::Offline => ProviderCatalogSource::Offline,
                        },
                        auth: ProviderAuthState::Unauthenticated,
                        result: ProviderCatalogResult::Models,
                    });
                }
            }
        }
        providers.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
        providers.dedup_by(|left, right| left.provider_id == right.provider_id);

        let default_model = requested_default
            .and_then(|requested| {
                let requested = requested.as_str();
                if let Some(model) = models.iter().find(|model| {
                    requested
                        .strip_prefix(model.provider_id.as_str())
                        .and_then(|suffix| suffix.strip_prefix('/'))
                        == Some(model.model_id.as_str())
                }) {
                    return Some(model.model_ref());
                }
                let mut bare_matches = models.iter().filter(|model| model.model_id == requested);
                let model = bare_matches.next()?;
                bare_matches.next().is_none().then(|| model.model_ref())
            })
            .or_else(|| models.first().map(ProviderModel::model_ref))
            .unwrap_or_else(|| ModelRef::new("hya/offline"));

        Self {
            models: Arc::from(models),
            providers: Arc::from(providers),
            default_model,
            notice,
        }
    }

    /// Borrow all normalized model rows without cloning the snapshot.
    #[must_use]
    pub fn models(&self) -> &[ProviderModel] {
        &self.models
    }

    /// Borrow all declared provider statuses without cloning the snapshot.
    #[must_use]
    pub fn providers(&self) -> &[ProviderCatalogState] {
        &self.providers
    }

    /// Borrow the row-backed process default.
    #[must_use]
    pub fn default_model(&self) -> &ModelRef {
        &self.default_model
    }

    /// Borrow the optional offline configuration notice.
    #[must_use]
    pub fn notice(&self) -> Option<&CatalogNotice> {
        self.notice.as_ref()
    }
}
