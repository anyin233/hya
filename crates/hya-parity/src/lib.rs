//! `hya_parity` — TS-vs-Rust parity harness (tmux capture + normalize + golden diff),
//! used by the per-wave proofs and the W10 full parity matrix (PLAN.md).
//!
//! It also hosts the W0 contract-stability test: importing and constructing every frozen
//! contract means an incompatible shape change breaks compilation here, flagging a
//! contract-owner change before fan-out drifts.

pub mod capture;
pub mod golden;
pub mod tmux;

#[cfg(test)]
mod w0_contracts {
    use hya_sdk::{Client, Data, EventPayload, GlobalEvent, V2Event};
    use hya_tui::{BindingId, Key, KeyEvent, LayoutResult, PromptDoc, RenderNode, Rgba};

    // Object-safety of the SDK Client trait is part of the contract (`Arc<dyn Client>`).
    fn _assert_client_object_safe(_c: &dyn Client) {}

    #[test]
    fn frozen_contracts_present_and_constructible() {
        // sdk: reducer + events + wire envelope
        let mut data = Data::default();
        let ev = V2Event::PromptAdmitted {
            session_id: "ses_x".into(),
        };
        hya_sdk::reducer::apply(&mut data, &ev);
        let payload = EventPayload {
            id: None,
            kind: "server.connected".into(),
            properties: serde_json::json!({}),
        };
        let ge = GlobalEvent {
            directory: None,
            project: None,
            workspace: None,
            payload,
        };
        assert!(!ge.is_sync_envelope());

        // tui: color + prompt doc + input + render contract
        assert_eq!(
            Rgba::TRANSPARENT.over(Rgba::rgb(1, 2, 3)),
            Rgba::rgb(1, 2, 3)
        );
        let _doc = PromptDoc::default();
        let _bind = BindingId("session.list".into());
        let _key = KeyEvent::new(Key::Enter);
        let _node = RenderNode::default();
        let _layout = LayoutResult::default();
    }
}
