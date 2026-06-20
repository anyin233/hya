use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;
use yaca_proto::MemberId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamState {
    Creating,
    Active,
    ShutdownRequested,
    Draining,
    Completed,
    Failed,
    ForceDeleting,
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberState {
    Spawning,
    Active,
    ClosureReady,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamEvent {
    AllSpawned,
    ShutdownRequest,
    AllApproved,
    ShutdownRejected,
    AllMembersDone,
    ForceDelete,
    Delete,
}

/// The typed transition table (design.md §8). `None` = an invalid transition that
/// the caller rejects at the command boundary.
#[must_use]
pub fn team_transition(from: TeamState, event: TeamEvent) -> Option<TeamState> {
    use TeamEvent as E;
    use TeamState as S;
    Some(match (from, event) {
        (S::Creating, E::AllSpawned) => S::Active,
        (S::Active, E::ShutdownRequest) => S::ShutdownRequested,
        (S::ShutdownRequested, E::AllApproved) => S::Draining,
        (S::ShutdownRequested, E::ShutdownRejected) => S::Active,
        (S::Draining, E::AllMembersDone) => S::Completed,
        (S::Creating | S::Active | S::ShutdownRequested | S::Draining, E::ForceDelete) => {
            S::ForceDeleting
        }
        (S::Completed | S::Failed, E::Delete) | (S::ForceDeleting, E::Delete) => S::Deleted,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailEndpoint {
    Lead,
    Member(MemberId),
    Broadcast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    Message,
    Announcement,
}

#[derive(Clone, Debug, Serialize)]
pub struct MailEnvelope {
    pub id: u64,
    pub from: MailEndpoint,
    pub to: MailEndpoint,
    pub kind: MailKind,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Claimed,
    InProgress,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct TaskItem {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub status: TaskStatus,
    pub assignee: Option<MemberId>,
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum TeamError {
    #[error("invalid transition from {0:?}")]
    InvalidTransition(TeamState),
    #[error("broadcast is lead-only")]
    BroadcastNotLead,
    #[error("members still active")]
    MembersActive,
    #[error("not found")]
    NotFound,
}

#[derive(Clone, Debug, Serialize)]
pub struct TeamStatusSnapshot {
    pub state: TeamState,
    pub members: Vec<(String, MemberState)>,
    pub unread: usize,
    pub open_tasks: usize,
}

pub struct TeamControlPlane {
    state: TeamState,
    members: BTreeMap<MemberId, MemberState>,
    mailbox: Vec<MailEnvelope>,
    tasks: Vec<TaskItem>,
    next_mail: u64,
    next_task: u64,
}

impl TeamControlPlane {
    #[must_use]
    pub fn new(member_ids: &[MemberId]) -> Self {
        let members = member_ids
            .iter()
            .map(|id| (*id, MemberState::Spawning))
            .collect();
        Self {
            state: TeamState::Creating,
            members,
            mailbox: Vec::new(),
            tasks: Vec::new(),
            next_mail: 1,
            next_task: 1,
        }
    }

    #[must_use]
    pub fn state(&self) -> TeamState {
        self.state
    }

    fn apply(&mut self, event: TeamEvent) -> Result<(), TeamError> {
        match team_transition(self.state, event) {
            Some(next) => {
                self.state = next;
                Ok(())
            }
            None => Err(TeamError::InvalidTransition(self.state)),
        }
    }

    pub fn mark_member(&mut self, id: MemberId, member_state: MemberState) {
        self.members.entry(id).and_modify(|s| *s = member_state);
        if self.state == TeamState::Creating
            && self.members.values().all(|s| *s != MemberState::Spawning)
        {
            let _ = self.apply(TeamEvent::AllSpawned);
        }
    }

    pub fn send_message(
        &mut self,
        from: MailEndpoint,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
    ) -> Result<u64, TeamError> {
        if to == MailEndpoint::Broadcast && from != MailEndpoint::Lead {
            return Err(TeamError::BroadcastNotLead);
        }
        let id = self.next_mail;
        self.next_mail += 1;
        self.mailbox.push(MailEnvelope {
            id,
            from,
            to,
            kind,
            body,
        });
        Ok(id)
    }

    #[must_use]
    pub fn poll(&self, recipient: MailEndpoint, after: u64) -> Vec<MailEnvelope> {
        self.mailbox
            .iter()
            .filter(|m| m.id > after && (m.to == recipient || m.to == MailEndpoint::Broadcast))
            .cloned()
            .collect()
    }

    pub fn task_create(&mut self, title: String, body: String) -> u64 {
        let id = self.next_task;
        self.next_task += 1;
        self.tasks.push(TaskItem {
            id,
            title,
            body,
            status: TaskStatus::Open,
            assignee: None,
        });
        id
    }

    pub fn task_update(
        &mut self,
        id: u64,
        status: TaskStatus,
        assignee: Option<MemberId>,
    ) -> Result<(), TeamError> {
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(TeamError::NotFound)?;
        task.status = status;
        if assignee.is_some() {
            task.assignee = assignee;
        }
        Ok(())
    }

    #[must_use]
    pub fn task_get(&self, id: u64) -> Option<&TaskItem> {
        self.tasks.iter().find(|t| t.id == id)
    }

    #[must_use]
    pub fn task_list(&self) -> &[TaskItem] {
        &self.tasks
    }

    pub fn shutdown_request(&mut self) -> Result<(), TeamError> {
        self.apply(TeamEvent::ShutdownRequest)
    }

    pub fn approve_shutdown(&mut self, id: MemberId) -> Result<(), TeamError> {
        self.members
            .entry(id)
            .and_modify(|s| *s = MemberState::Done);
        if self.members.values().all(|s| {
            matches!(
                s,
                MemberState::Done | MemberState::Failed | MemberState::ClosureReady
            )
        }) {
            self.apply(TeamEvent::AllApproved)?;
            self.apply(TeamEvent::AllMembersDone)?;
        }
        Ok(())
    }

    pub fn reject_shutdown(&mut self) -> Result<(), TeamError> {
        self.apply(TeamEvent::ShutdownRejected)
    }

    pub fn delete(&mut self, force: bool) -> Result<(), TeamError> {
        if force {
            self.apply(TeamEvent::ForceDelete)?;
            self.apply(TeamEvent::Delete)
        } else {
            let active = self
                .members
                .values()
                .any(|s| matches!(s, MemberState::Spawning | MemberState::Active));
            if active {
                return Err(TeamError::MembersActive);
            }
            self.apply(TeamEvent::Delete)
        }
    }

    #[must_use]
    pub fn status(&self, recipient: MailEndpoint) -> TeamStatusSnapshot {
        TeamStatusSnapshot {
            state: self.state,
            members: self
                .members
                .iter()
                .map(|(id, s)| (id.to_string(), *s))
                .collect(),
            unread: self.poll(recipient, 0).len(),
            open_tasks: self
                .tasks
                .iter()
                .filter(|t| t.status == TaskStatus::Open)
                .count(),
        }
    }
}
