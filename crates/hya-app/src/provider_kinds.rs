//! Provider kinds the runtime routes: the core OpenAI-compatible kind plus
//! every kind built into this binary.

use std::sync::Arc;

use hya_provider::{
    AnthropicMessagesProtocol, GoogleProtocol, GrokBuildProtocol, OpenAiCodexProtocol,
    OpenAiResponsesProtocol, Protocol, ProviderKindEntry, ProviderKinds,
};

/// The core kinds and the five kinds `hya-provider` still compiles in.
///
/// # Errors
/// Two kinds share a label, alias, or OAuth type.
pub fn builtin() -> anyhow::Result<Arc<ProviderKinds>> {
    let mut kinds = ProviderKinds::core();
    let protocols: [Arc<dyn Protocol>; 5] = [
        Arc::new(OpenAiResponsesProtocol),
        Arc::new(OpenAiCodexProtocol),
        Arc::new(GrokBuildProtocol),
        Arc::new(AnthropicMessagesProtocol),
        Arc::new(GoogleProtocol),
    ];
    for protocol in protocols {
        kinds.register(
            "builtin",
            ProviderKindEntry {
                protocol,
                auth_flow: None,
            },
        )?;
    }
    Ok(Arc::new(kinds))
}
