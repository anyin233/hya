use hya_tool::{AskRequest, Decision};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub fn spawn_reject_responder(mut asks: mpsc::UnboundedReceiver<AskRequest>) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(req) = asks.recv().await {
            let _ = req.reply.send(Decision::Reject { feedback: None });
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_tool::{
        Invocation, InvocationPolicy, PermissionModel, PermissionPlane, PermissionRules,
    };
    #[tokio::test]
    async fn headless_responder_rejects_native_asks() {
        let (plane, asks) = PermissionPlane::new_with_policy(
            PermissionRules::default(),
            InvocationPolicy::compile(PermissionModel::Default, Vec::new()).unwrap(),
        );
        let _responder = spawn_reject_responder(asks);

        assert!(
            plane
                .authorize(&Invocation::tool("write", hya_tool::Mode::Ask))
                .await
                .is_err()
        );
    }
}
