//! Pin existing hook JSON before generating the cross-language contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use hya_plugin::messages::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

fn roundtrip<T: Serialize + DeserializeOwned>(value: &Value) {
    let decoded: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), *value);
}

#[test]
fn every_hook_keeps_its_wire_shape() {
    let fixtures: Value = serde_json::from_str(include_str!("../fixtures/hook-wire.json")).unwrap();
    for (name, fixture) in fixtures.as_object().unwrap() {
        let hook = HookName::from_wire(name).unwrap();
        assert_eq!(serde_json::to_value(hook).unwrap(), *name);
        assert_eq!(hook.as_str(), name);
        macro_rules! check {
            ($params:ty, $reply:ty) => {{
                roundtrip::<$params>(&fixture["params"]);
                if let Some(others) = fixture.get("other_params") {
                    for params in others.as_array().unwrap() {
                        roundtrip::<$params>(params);
                    }
                }
                for reply in fixture["replies"].as_array().unwrap() {
                    roundtrip::<$reply>(reply);
                }
            }};
        }
        match hook {
            HookName::Event => check!(EventNotificationParams, Value),
            HookName::CommandExecuteBefore => {
                check!(CommandExecuteBeforeParams, CommandBeforeOutcomeWire)
            }
            HookName::TextComplete => check!(TextCompleteParams, TextCompleteOutcomeWire),
            HookName::MessageUserBefore => {
                check!(MessageUserBeforeParams, MessageUserBeforeOutcomeWire)
            }
            HookName::ChatParams => check!(ChatParamsParams, ChatParamsOutcomeWire),
            HookName::ToolExecuteBefore => check!(ToolExecuteBeforeParams, ToolBeforeOutcomeWire),
            HookName::ToolExecuteAfter => check!(ToolExecuteAfterParams, ToolAfterOutcomeWire),
            HookName::PermissionAsk => check!(PermissionAskParams, PermissionOutcomeWire),
            HookName::PermissionApprove => check!(PermissionApproveParams, PermissionOutcomeWire),
            HookName::CompactionBefore => {
                check!(CompactionBeforeParams, CompactionBeforeOutcomeWire)
            }
            HookName::CompactionAfter => check!(CompactionAfterParams, Value),
            HookName::SessionStart | HookName::SessionEnd => check!(SessionLifecycleParams, Value),
            HookName::AgentSpawn => check!(AgentSpawnParams, Value),
            HookName::ModelFallback => check!(ModelFallbackParams, ModelFallbackOutcomeWire),
        }
    }
    assert_eq!(fixtures.as_object().unwrap().len(), HookName::ALL.len());
}

#[test]
fn bundle_declared_hook_points_are_registered_by_name() {
    let declared: HookRegistration =
        serde_json::from_value(serde_json::json!({ "name": "hya/goal:evaluate" })).unwrap();
    assert_eq!(declared.name, HookId::Declared("hya/goal:evaluate".into()));
    assert_eq!(declared.name.method(), "hook/hya/goal:evaluate");
    let core: HookRegistration =
        serde_json::from_value(serde_json::json!({ "name": "chat.params" })).unwrap();
    assert_eq!(core.name, HookId::Core(HookName::ChatParams));
    for unknown in ["goal.evaluate", ":point", "bundle:"] {
        assert!(
            serde_json::from_value::<HookRegistration>(serde_json::json!({ "name": unknown }))
                .is_err(),
            "{unknown}"
        );
    }
}
