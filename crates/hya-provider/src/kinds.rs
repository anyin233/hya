//! Registered provider kinds: the config `kind` labels this process can route.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use crate::oauth::ProviderAuthFlow;
use crate::{OpenAiChatProtocol, Protocol};

/// Source name of the kinds built into the core.
const CORE_SOURCE: &str = "core";

/// One provider kind: its wire protocol and, optionally, its OAuth flow.
#[derive(Clone)]
pub struct ProviderKindEntry {
    /// Encoder, decoder, headers and catalog adapter of the kind.
    pub protocol: Arc<dyn Protocol>,
    /// Login and refresh flow for credentials of this kind.
    pub auth_flow: Option<Arc<dyn ProviderAuthFlow>>,
}

/// A kind label, alias, or OAuth type that is already registered.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("provider {what} `{name}` from {bundle} is already registered by {existing}")]
pub struct KindConflict {
    /// `kind` or `OAuth type`.
    pub what: &'static str,
    /// The conflicting label or OAuth type.
    pub name: String,
    /// Source that tried to register it.
    pub bundle: String,
    /// Source that registered it first.
    pub existing: String,
}

/// Provider kinds by config label and alias, and OAuth flows by OAuth type.
#[derive(Clone)]
pub struct ProviderKinds {
    kinds: BTreeMap<String, (String, ProviderKindEntry)>,
    flows: BTreeMap<String, (String, Arc<dyn ProviderAuthFlow>)>,
}

impl ProviderKinds {
    /// The kinds the core always has: OpenAI-compatible Chat Completions.
    #[must_use]
    pub fn core() -> Self {
        let mut kinds = Self {
            kinds: BTreeMap::new(),
            flows: BTreeMap::new(),
        };
        kinds.insert(
            CORE_SOURCE,
            ProviderKindEntry {
                protocol: Arc::new(OpenAiChatProtocol),
                auth_flow: None,
            },
        );
        kinds
    }

    /// Register `entry` from `source_bundle` under its label, its aliases, and
    /// its flow's OAuth type. Nothing is registered when any of them is taken.
    ///
    /// # Errors
    /// [`KindConflict`] naming both sources.
    pub fn register(
        &mut self,
        source_bundle: &str,
        entry: ProviderKindEntry,
    ) -> Result<(), KindConflict> {
        let conflict = |what, name: &str, existing: &str| KindConflict {
            what,
            name: name.to_string(),
            bundle: source_bundle.to_string(),
            existing: existing.to_string(),
        };
        for label in labels_of(entry.protocol.as_ref()) {
            if let Some((existing, _)) = self.kinds.get(label) {
                return Err(conflict("kind", label, existing));
            }
        }
        if let Some(flow) = &entry.auth_flow
            && let Some((existing, _)) = self.flows.get(flow.oauth_type())
        {
            return Err(conflict("OAuth type", flow.oauth_type(), existing));
        }
        self.insert(source_bundle, entry);
        Ok(())
    }

    fn insert(&mut self, source: &str, entry: ProviderKindEntry) {
        for label in labels_of(entry.protocol.as_ref()) {
            self.kinds
                .insert(label.to_string(), (source.to_string(), entry.clone()));
        }
        if let Some(flow) = &entry.auth_flow {
            self.flows.insert(
                flow.oauth_type().to_string(),
                (source.to_string(), Arc::clone(flow)),
            );
        }
    }

    /// Protocol registered under `label` (a kind or one of its aliases).
    #[must_use]
    pub fn protocol(&self, label: &str) -> Option<&Arc<dyn Protocol>> {
        self.kinds.get(label).map(|(_, entry)| &entry.protocol)
    }

    /// OAuth flow registered for `oauth_type`.
    #[must_use]
    pub fn auth_flow(&self, oauth_type: &str) -> Option<&Arc<dyn ProviderAuthFlow>> {
        self.flows.get(oauth_type).map(|(_, flow)| flow)
    }

    /// Every accepted `kind` label, aliases included, sorted.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.kinds.keys().map(String::as_str)
    }

    /// Whether the kind under `label` has an OAuth flow.
    #[must_use]
    pub fn supports_oauth(&self, label: &str) -> bool {
        self.kinds
            .get(label)
            .is_some_and(|(_, entry)| entry.auth_flow.is_some())
    }
}

fn labels_of(protocol: &dyn Protocol) -> impl Iterator<Item = &str> {
    std::iter::once(protocol.kind()).chain(protocol.aliases().iter().copied())
}
