use std::collections::HashMap;

use async_trait::async_trait;
use hya_core::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, TextCompleteInput,
    TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use hya_proto::Envelope;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::client::{DEFAULT_CALL_TIMEOUT, PluginClient};
use crate::dispatcher::{outcome_to_wire, wire_to_outcome};
use crate::messages::{
    EventNotificationParams, HookName, HookPosture, HookRegistration, METHOD_EVENT,
    ToolAfterOutcomeWire, ToolBeforeOutcomeWire, ToolExecuteAfterParams, ToolExecuteBeforeParams,
};

const EVENT_CHANNEL_CAP: usize = 256;
const GUARD_FAILED_SAFE: &str = "guard failed safe";

/// Hook dispatcher bound to one already-initialized raw plugin client.
pub struct ActivationHookDispatcher {
    client: PluginClient,
    hooks: HashMap<HookName, HookPosture>,
    event_tx: Option<mpsc::Sender<Envelope>>,
}

impl ActivationHookDispatcher {
    #[must_use]
    pub fn new(client: PluginClient, registrations: &[HookRegistration]) -> Self {
        let hooks = registrations
            .iter()
            .map(|registration| {
                (
                    registration.name,
                    resolved_posture(registration.name, registration.posture),
                )
            })
            .collect::<HashMap<_, _>>();
        let event_tx = hooks
            .contains_key(&HookName::Event)
            .then(|| spawn_event_drain(client.clone()));
        Self {
            client,
            hooks,
            event_tx,
        }
    }

    fn posture(&self, hook: HookName) -> Option<HookPosture> {
        self.hooks.get(&hook).copied()
    }
}

fn resolved_posture(hook: HookName, declared: Option<HookPosture>) -> HookPosture {
    let default = hook.default_posture();
    match (default, declared) {
        (HookPosture::Safe, _) | (_, Some(HookPosture::Safe)) => HookPosture::Safe,
        (_, Some(HookPosture::Open) | None) => HookPosture::Open,
    }
}

fn spawn_event_drain(client: PluginClient) -> mpsc::Sender<Envelope> {
    let (tx, mut rx) = mpsc::channel(EVENT_CHANNEL_CAP);
    tokio::spawn(async move {
        while let Some(envelope) = rx.recv().await {
            let params = match serde_json::to_value(EventNotificationParams { envelope }) {
                Ok(params) => params,
                Err(error) => {
                    tracing::warn!(%error, "activation event notification serialize failed");
                    break;
                }
            };
            if let Err(error) = client.notify(METHOD_EVENT, params).await {
                tracing::warn!(%error, "activation event notification failed");
                break;
            }
        }
    });
    tx
}

fn before_failure(
    posture: HookPosture,
    input: Value,
    error: impl std::fmt::Display,
) -> ToolExecuteBeforeOutcome {
    match posture {
        HookPosture::Safe => ToolExecuteBeforeOutcome::Veto {
            reason: format!("{GUARD_FAILED_SAFE}: {error}"),
        },
        HookPosture::Open => ToolExecuteBeforeOutcome::Continue { input },
    }
}

#[async_trait]
impl HookDispatcher for ActivationHookDispatcher {
    fn dispatch_event(&self, envelope: &Envelope) {
        let Some(event_tx) = &self.event_tx else {
            return;
        };
        if let Err(error) = event_tx.try_send(envelope.clone()) {
            tracing::warn!(%error, "activation event notification backpressure or closed");
        }
    }

    fn is_healthy(&self) -> bool {
        !self.client.is_closed()
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        let Some(posture) = self.posture(HookName::ToolExecuteBefore) else {
            return ToolExecuteBeforeOutcome::Continue { input: input.input };
        };
        let original_input = input.input;
        let params = ToolExecuteBeforeParams {
            session: input.session,
            message: input.message,
            call: input.call,
            tool: input.tool,
            input: original_input.clone(),
        };
        let params = match serde_json::to_value(params) {
            Ok(params) => params,
            Err(error) => return before_failure(posture, original_input, error),
        };
        match self
            .client
            .call(
                &HookName::ToolExecuteBefore.method(),
                params,
                DEFAULT_CALL_TIMEOUT,
            )
            .await
        {
            Ok(value) => match serde_json::from_value::<ToolBeforeOutcomeWire>(value) {
                Ok(ToolBeforeOutcomeWire::Continue { input }) => {
                    ToolExecuteBeforeOutcome::Continue { input }
                }
                Ok(ToolBeforeOutcomeWire::Veto { reason }) => {
                    ToolExecuteBeforeOutcome::Veto { reason }
                }
                Err(error) => before_failure(posture, original_input, error),
            },
            Err(error) => before_failure(posture, original_input, error),
        }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        if self.posture(HookName::ToolExecuteAfter).is_none() {
            return ToolExecuteAfterOutcome::Continue {
                result: input.result,
            };
        }
        let original_result = outcome_to_wire(input.result);
        let params = ToolExecuteAfterParams {
            session: input.session,
            message: input.message,
            call: input.call,
            tool: input.tool,
            input: input.input,
            result: original_result.clone(),
        };
        let params = match serde_json::to_value(params) {
            Ok(params) => params,
            Err(_) => {
                return ToolExecuteAfterOutcome::Continue {
                    result: wire_to_outcome(original_result),
                };
            }
        };
        let value = match self
            .client
            .call(
                &HookName::ToolExecuteAfter.method(),
                params,
                DEFAULT_CALL_TIMEOUT,
            )
            .await
        {
            Ok(value) => value,
            Err(_) => {
                return ToolExecuteAfterOutcome::Continue {
                    result: wire_to_outcome(original_result),
                };
            }
        };
        match serde_json::from_value::<ToolAfterOutcomeWire>(value) {
            Ok(ToolAfterOutcomeWire::Continue { result }) => ToolExecuteAfterOutcome::Continue {
                result: wire_to_outcome(result),
            },
            Err(_) => ToolExecuteAfterOutcome::Continue {
                result: wire_to_outcome(original_result),
            },
        }
    }
}
