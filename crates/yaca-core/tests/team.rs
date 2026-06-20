#![allow(clippy::unwrap_used, clippy::expect_used)]

use yaca_core::team::{TeamEvent, TeamStatusSnapshot};
use yaca_core::{
    MailEndpoint, MailKind, MemberState, TaskStatus, TeamControlPlane, TeamError, TeamState,
    team_transition,
};
use yaca_proto::MemberId;

#[test]
fn state_machine_accepts_valid_and_rejects_invalid() {
    assert_eq!(
        team_transition(TeamState::Creating, TeamEvent::AllSpawned),
        Some(TeamState::Active)
    );
    assert_eq!(
        team_transition(TeamState::Active, TeamEvent::ShutdownRequest),
        Some(TeamState::ShutdownRequested)
    );
    assert_eq!(
        team_transition(TeamState::ShutdownRequested, TeamEvent::ShutdownRejected),
        Some(TeamState::Active)
    );
    assert_eq!(
        team_transition(TeamState::Active, TeamEvent::ForceDelete),
        Some(TeamState::ForceDeleting)
    );
    // invalid: cannot plain-delete an Active team
    assert_eq!(team_transition(TeamState::Active, TeamEvent::Delete), None);
    // invalid: cannot shutdown a Creating team
    assert_eq!(
        team_transition(TeamState::Creating, TeamEvent::ShutdownRequest),
        None
    );
}

#[test]
fn graceful_lifecycle_create_to_deleted() {
    let m1 = MemberId::new();
    let m2 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1, m2]);
    assert_eq!(team.state(), TeamState::Creating);

    team.mark_member(m1, MemberState::Active);
    team.mark_member(m2, MemberState::Active);
    assert_eq!(team.state(), TeamState::Active);

    team.shutdown_request().unwrap();
    assert_eq!(team.state(), TeamState::ShutdownRequested);

    team.approve_shutdown(m1).unwrap();
    team.approve_shutdown(m2).unwrap();
    assert_eq!(team.state(), TeamState::Completed);

    team.delete(false).unwrap();
    assert_eq!(team.state(), TeamState::Deleted);
}

#[test]
fn plain_delete_rejects_active_members_but_force_succeeds() {
    let m1 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1]);
    team.mark_member(m1, MemberState::Active);
    assert_eq!(team.state(), TeamState::Active);

    assert_eq!(team.delete(false), Err(TeamError::MembersActive));
    assert_eq!(team.state(), TeamState::Active);

    team.delete(true).unwrap();
    assert_eq!(team.state(), TeamState::Deleted);
}

#[test]
fn mailbox_broadcast_is_lead_only() {
    let m1 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1]);

    let id = team
        .send_message(
            MailEndpoint::Lead,
            MailEndpoint::Member(m1),
            MailKind::Message,
            "do it".to_string(),
        )
        .unwrap();
    assert_eq!(id, 1);

    // member -> broadcast is rejected
    assert_eq!(
        team.send_message(
            MailEndpoint::Member(m1),
            MailEndpoint::Broadcast,
            MailKind::Announcement,
            "hi all".to_string(),
        ),
        Err(TeamError::BroadcastNotLead)
    );

    // lead broadcast is allowed and delivered to members on poll
    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Broadcast,
        MailKind::Announcement,
        "standup".to_string(),
    )
    .unwrap();
    let inbox = team.poll(MailEndpoint::Member(m1), 0);
    assert_eq!(inbox.len(), 2);
}

#[test]
fn task_board_create_update_get() {
    let mut team = TeamControlPlane::new(&[]);
    let t = team.task_create("write parser".to_string(), "details".to_string());
    assert_eq!(team.task_get(t).unwrap().status, TaskStatus::Open);

    team.task_update(t, TaskStatus::Completed, None).unwrap();
    assert_eq!(team.task_get(t).unwrap().status, TaskStatus::Completed);
    assert_eq!(
        team.task_update(999, TaskStatus::Open, None),
        Err(TeamError::NotFound)
    );

    let snap: TeamStatusSnapshot = team.status(MailEndpoint::Lead);
    assert_eq!(snap.open_tasks, 0);
}
