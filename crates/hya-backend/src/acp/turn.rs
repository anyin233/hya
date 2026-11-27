//! Project one admitted user turn into ordered editor updates. The v1 mirror
//! reconciles live deltas with their durable replacements.
use agent_client_protocol::{Result, v1::schema as acp};
use futures::{
    FutureExt, StreamExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use hya_api::v2 as pb;
use hya_sdk::SessionMirror;
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

use super::{Editor, Session, api};

pub(super) async fn run(
    session: &Session,
    client: Editor,
    text: String,
    cancel: &CancellationToken,
) -> Result<acp::StopReason> {
    let watermark = session
        .sdk
        .get_session(&session.id)
        .await
        .map_err(api)?
        .last_seq;
    let live = session
        .sdk
        .stream_session(&session.id, watermark)
        .await
        .map_err(api)?;
    if cancel.is_cancelled() {
        return Ok(acp::StopReason::Cancelled);
    }
    let prompt = pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
        text,
        attachments: Vec::new(),
    });
    let turn = session
        .sdk
        .create_turn(&session.id, prompt)
        .await
        .map_err(api)?;
    let mut output = Output {
        session,
        client,
        turn: turn.id.clone(),
        mirror: SessionMirror::from_messages(Vec::new(), watermark),
        within: false,
        updates: Updates::default(),
        answered: HashSet::new(),
        replies: FuturesUnordered::new(),
        tree: crate::routed::Asks::new(session.id.clone()),
    };
    let result = output.drive(live, cancel).await;
    if result.is_err() {
        let _ = session
            .sdk
            .cancel_matching_turn(&session.id, &turn.id)
            .await;
    }
    result
}

struct Output<'a> {
    session: &'a Session,
    client: Editor,
    turn: String,
    within: bool,
    mirror: SessionMirror,
    updates: Updates,
    answered: HashSet<String>,
    replies: FuturesUnordered<BoxFuture<'a, Result<()>>>,
    tree: crate::routed::Asks,
}

impl Output<'_> {
    async fn drive(
        &mut self,
        mut live: BoxStream<'_, std::result::Result<pb::StreamFrame, hya_sdk::SdkError>>,
        cancel: &CancellationToken,
    ) -> Result<acp::StopReason> {
        let sdk = &self.session.sdk;
        let session = self.session.id.clone();
        let turn = self.turn.clone();
        let mut cancelling = false;
        loop {
            let wait = sdk.wait_turn(&session, &turn, Duration::from_millis(350));
            tokio::pin!(wait);
            let mut reconnect = false;
            let info = loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled(), if !cancelling => {
                        cancelling = true;
                        sdk.cancel_matching_turn(&session, &turn).await.map_err(api)?;
                    }
                    Some(reply) = self.replies.next(), if !self.replies.is_empty() => reply?,
                    info = &mut wait => break info.map_err(api)?,
                    frame = live.next(), if !reconnect => match frame {
                        Some(Ok(pb::StreamFrame { frame:Some(pb::stream_frame::Frame::Event(event)) })) => self.event(event)?,
                        Some(Ok(_)) => { self.lost_live(); self.replay().await?; }
                        _ => { self.lost_live(); reconnect = true; }
                    }
                }
            };
            if let Ok(pb::TurnState::Finished | pb::TurnState::Cancelled | pb::TurnState::Failed) =
                pb::TurnState::try_from(info.state)
            {
                self.replay().await?;
                if cancelling
                    || cancel.is_cancelled()
                    || info.state == pb::TurnState::Cancelled as i32
                {
                    return Ok(acp::StopReason::Cancelled);
                }
                if info.state == pb::TurnState::Failed as i32 {
                    return Err(super::turn_error(info.error_code, info.error_message));
                }
                return Ok(if info.finish == pb::FinishReason::Length as i32 {
                    acp::StopReason::MaxTokens
                } else {
                    acp::StopReason::EndTurn
                });
            }
            if reconnect {
                live = sdk
                    .stream_session(&session, self.mirror.last_seq)
                    .await
                    .map_err(api)?;
                self.replay().await?;
            }
            if self.within && !cancelling {
                for ask in sdk.list_interactions().await.map_err(api)? {
                    if self.tree.belongs(sdk, &ask.session).await {
                        self.ask(ask);
                    }
                }
            }
        }
    }

    fn lost_live(&mut self) {
        self.mirror.apply(&pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Resync(pb::ResyncFrame {
                last_seq: self.mirror.last_seq,
            })),
        });
    }
    async fn replay(&mut self) -> Result<()> {
        let mut since = self.mirror.last_seq;
        loop {
            let page = self
                .session
                .sdk
                .list_events(&self.session.id, since)
                .await
                .map_err(api)?;
            if page.next_seq <= since {
                break;
            }
            since = page.next_seq;
            for event in page.events {
                self.event(event)?;
            }
            self.mirror.last_seq = since;
        }
        Ok(())
    }

    fn event(&mut self, event: pb::StreamEvent) -> Result<()> {
        use pb::stream_event::Payload as P;
        if event.seq != 0 && event.seq <= self.mirror.last_seq {
            return Ok(());
        }
        match &event.payload {
            // Resident wakes append user messages inside this turn; WaitTurn
            // reports where it ends.
            Some(P::MessageStarted(m)) if m.message == self.turn => self.within = true,
            Some(P::PermissionRequested(p)) if self.within => {
                if let Some(ask) = &p.interaction {
                    self.ask(ask.clone());
                }
            }
            Some(P::QuestionRequested(q)) if self.within => {
                if let Some(ask) = &q.interaction {
                    self.ask(ask.clone());
                }
            }
            _ => {}
        }
        if !self.within {
            self.mirror.last_seq = self.mirror.last_seq.max(event.seq);
            return Ok(());
        }
        let frame = pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Event(event)),
        };
        self.mirror.apply(&frame);
        let Some(pb::stream_frame::Frame::Event(event)) = &frame.frame else {
            return Ok(());
        };
        let message_id = match &event.payload {
            Some(P::PartStarted(p)) => &p.message,
            Some(P::PartAppended(p)) => &p.message,
            Some(P::PartReplaced(p)) => &p.message,
            Some(P::PartsAdded(p)) => &p.message,
            Some(P::ToolStateChanged(p)) => &p.message,
            _ => return Ok(()),
        };
        if let Some(message) = self.mirror.message(message_id)
            && message.role == pb::Role::Assistant as i32
        {
            for update in message
                .parts
                .iter()
                .filter_map(|part| self.updates.part(part))
            {
                self.client
                    .send_notification(acp::SessionNotification::new(
                        self.session.id.clone(),
                        update,
                    ))?;
            }
        }
        Ok(())
    }

    fn ask(&mut self, ask: pb::Interaction) {
        if !self.answered.insert(ask.id.clone()) {
            return;
        }
        self.replies
            .push(answer(self.session, self.client.clone(), ask).boxed());
    }
}

async fn answer(session: &Session, client: Editor, ask: pb::Interaction) -> Result<()> {
    use pb::respond_interaction_request::Response;
    let response = if ask.r#type == pb::InteractionType::Permission as i32 {
        let payload = serde_json::to_value(&ask.payload).unwrap_or_default();
        let call = acp::ToolCallUpdate::new(
            payload["callId"].as_str().unwrap_or(&ask.id).to_owned(),
            acp::ToolCallUpdateFields::new()
                .title(ask.title)
                .raw_input(payload.get("input").cloned()),
        );
        let req = acp::RequestPermissionRequest::new(
            session.id.clone(),
            call,
            vec![
                acp::PermissionOption::new(
                    "allow",
                    "Allow once",
                    acp::PermissionOptionKind::AllowOnce,
                ),
                acp::PermissionOption::new(
                    "deny",
                    "Reject once",
                    acp::PermissionOptionKind::RejectOnce,
                ),
            ],
        );
        let answer = client.send_request(req).block_task().await;
        let allowed = matches!(answer.map(|r|r.outcome),Ok(acp::RequestPermissionOutcome::Selected(selected)) if selected.option_id.0.as_ref() == "allow");
        Response::Permission(pb::PermissionResponse {
            allowed,
            persist: false,
        })
    } else {
        Response::Question(pb::QuestionResponse {
            rejected: true,
            answer: String::new(),
        })
    };
    match session.sdk.respond_interaction(&ask.id, response).await {
        Ok(_) => Ok(()),
        Err(hya_sdk::SdkError::Api { code, .. }) if code == "not_found" => Ok(()),
        Err(e) => Err(api(e)),
    }
}

/// Track only what the editor has seen. The SDK mirror owns part contents.
#[derive(Default)]
struct Updates {
    sent_text: HashMap<String, usize>,
    tool_states: HashMap<String, i32>,
}

impl Updates {
    fn part(&mut self, part: &pb::PartInfo) -> Option<acp::SessionUpdate> {
        let update = match &part.kind {
            Some(pb::part_info::Kind::Text(t)) => {
                let prior = self.sent_text.entry(part.id.clone()).or_default();
                let delta = t.text.get(*prior..)?;
                if delta.is_empty() {
                    return None;
                }
                let chunk = acp::ContentChunk::new(delta.to_owned().into());
                *prior = t.text.len();
                acp::SessionUpdate::AgentMessageChunk(chunk)
            }
            Some(pb::part_info::Kind::ToolCall(t)) if !t.call_id.is_empty() => {
                let previous = self.tool_states.insert(t.call_id.clone(), t.state);
                if previous == Some(t.state) {
                    return None;
                }
                let status = match pb::ToolExecutionState::try_from(t.state) {
                    Ok(pb::ToolExecutionState::Running) => acp::ToolCallStatus::InProgress,
                    Ok(pb::ToolExecutionState::Ok) => acp::ToolCallStatus::Completed,
                    Ok(pb::ToolExecutionState::Error | pb::ToolExecutionState::Denied) => {
                        acp::ToolCallStatus::Failed
                    }
                    _ => acp::ToolCallStatus::Pending,
                };
                let input = serde_json::from_str::<serde_json::Value>(&t.input_json).ok();
                let output = serde_json::from_str::<serde_json::Value>(&t.output_json).ok();
                if previous.is_none() {
                    acp::SessionUpdate::ToolCall(
                        acp::ToolCall::new(t.call_id.clone(), &t.tool)
                            .status(status)
                            .raw_input(input)
                            .raw_output(output),
                    )
                } else {
                    acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(
                        t.call_id.clone(),
                        acp::ToolCallUpdateFields::new()
                            .status(status)
                            .title(t.tool.clone())
                            .raw_input(input)
                            .raw_output(output),
                    ))
                }
            }
            _ => return None,
        };
        Some(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_text_and_replayed_replacements_emit_one_ordered_suffix() {
        let mut updates = Updates::default();
        let mut chunks = Vec::new();
        for text in ["", "你", "你好", "你好", "你好！"] {
            let part = pb::PartInfo {
                id: "part".into(),
                kind: Some(pb::part_info::Kind::Text(pb::TextPart {
                    text: text.into(),
                })),
            };
            if let Some(acp::SessionUpdate::AgentMessageChunk(chunk)) = updates.part(&part)
                && let acp::ContentBlock::Text(text) = chunk.content
            {
                chunks.push(text.text);
            }
        }
        assert_eq!(chunks, ["你", "好", "！"]);
    }
}
