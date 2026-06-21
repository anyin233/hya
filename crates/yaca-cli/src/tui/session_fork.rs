use anyhow::Context as _;
use yaca_core::{AgentSpec, CreateSession, SessionEngine};
use yaca_proto::{Envelope, Event, PartProjection, Projection, Role, SessionId};

use super::block_action::{SelectedBlockAction, SelectedBlockActionKind};
use super::history::HistoryStore;

mod events;

use events::{event_message_id, rewrite_session};

pub struct ForkedSession {
    pub session: SessionId,
    pub projection: Projection,
    pub prompt_input: String,
}

pub async fn fork_selected_block(
    engine: &SessionEngine,
    history: &HistoryStore,
    current: SessionId,
    agent: &AgentSpec,
    projection: &Projection,
    action: SelectedBlockAction,
) -> anyhow::Result<Option<ForkedSession>> {
    let replay = engine
        .replay(current)
        .await
        .context("replay current session")?;
    let Some(events) = prefix_events_for_action(projection, &replay, action) else {
        return Ok(None);
    };
    let prompt_input = prompt_input_for_action(projection, action);
    let title = action_title(action);
    let fork = engine
        .create(CreateSession {
            parent: Some(current),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .context("create selected block session")?;
    for event in events {
        let rewritten = rewrite_session(&event, fork);
        engine
            .store()
            .append_event(fork, &rewritten)
            .await
            .context("append forked event")?;
    }
    engine
        .store()
        .append_event(
            fork,
            &Event::SessionTitled {
                session: fork,
                title,
            },
        )
        .await
        .context("title forked session")?;
    let projection = engine
        .read_projection(fork)
        .await
        .context("read forked projection")?;
    history
        .create_session(
            fork,
            agent.model.as_str(),
            agent.name.as_str(),
            &agent.workdir.to_string_lossy(),
        )
        .context("create fork history")?;
    for env in engine.replay(fork).await.context("replay forked session")? {
        history
            .append_envelope(fork, &env)
            .context("append fork history")?;
    }
    Ok(Some(ForkedSession {
        session: fork,
        projection,
        prompt_input,
    }))
}

fn action_title(action: SelectedBlockAction) -> String {
    let ordinal = action.message_index.saturating_add(1);
    match action.kind {
        SelectedBlockActionKind::Revert => format!("Reverted to block #{ordinal}"),
        SelectedBlockActionKind::Branch => format!("Branch from block #{ordinal}"),
    }
}

fn prefix_events_for_selection(
    projection: &Projection,
    replay: &[Envelope],
    selected: usize,
) -> Option<Vec<Event>> {
    let target = projection.session.messages.get(selected)?.id;
    let cutoff = replay
        .iter()
        .filter(|env| event_message_id(&env.event) == Some(target))
        .map(|env| env.seq.0)
        .max()?;
    let events = replay
        .iter()
        .take_while(|env| env.seq.0 <= cutoff)
        .filter_map(|env| match env.event {
            Event::SessionCreated { .. } => None,
            _ => Some(env.event.clone()),
        })
        .collect::<Vec<_>>();
    Some(events)
}

fn prefix_events_for_action(
    projection: &Projection,
    replay: &[Envelope],
    action: SelectedBlockAction,
) -> Option<Vec<Event>> {
    match action.kind {
        SelectedBlockActionKind::Branch => {
            prefix_events_for_selection(projection, replay, action.message_index)
        }
        SelectedBlockActionKind::Revert => {
            prefix_events_before_selection(projection, replay, action.message_index)
        }
    }
}

fn prefix_events_before_selection(
    projection: &Projection,
    replay: &[Envelope],
    selected: usize,
) -> Option<Vec<Event>> {
    let target = projection.session.messages.get(selected)?.id;
    let cutoff = replay
        .iter()
        .filter(|env| event_message_id(&env.event) == Some(target))
        .map(|env| env.seq.0)
        .min()?;
    let events = replay
        .iter()
        .take_while(|env| env.seq.0 < cutoff)
        .filter_map(|env| match env.event {
            Event::SessionCreated { .. } => None,
            _ => Some(env.event.clone()),
        })
        .collect::<Vec<_>>();
    Some(events)
}

fn prompt_input_for_action(projection: &Projection, action: SelectedBlockAction) -> String {
    if action.kind != SelectedBlockActionKind::Revert {
        return String::new();
    }
    let Some(message) = projection.session.messages.get(action.message_index) else {
        return String::new();
    };
    if message.role != Role::User {
        return String::new();
    }
    let mut text = String::new();
    for part in &message.parts {
        if let PartProjection::Text {
            text: part_text, ..
        } = part
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(part_text);
        }
    }
    text
}

#[cfg(test)]
mod tests;
