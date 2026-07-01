#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_core::team::{TeamEvent, TeamStatusSnapshot};
use hya_core::{
    MailEndpoint, MailKind, MemberState, TaskStatus, TeamControlPlane, TeamError, TeamState,
    team_transition,
};
use hya_proto::MemberId;

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

#[test]
fn mailbox_poll_after_excludes_older_and_non_recipient_messages() {
    let m1 = MemberId::new();
    let m2 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1, m2]);

    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Member(m1),
        MailKind::Message,
        "first for m1".to_string(),
    )
    .unwrap();
    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Member(m2),
        MailKind::Message,
        "only for m2".to_string(),
    )
    .unwrap();
    let broadcast = team
        .send_message(
            MailEndpoint::Lead,
            MailEndpoint::Broadcast,
            MailKind::Announcement,
            "for everyone".to_string(),
        )
        .unwrap();
    let newer_direct = team
        .send_message(
            MailEndpoint::Lead,
            MailEndpoint::Member(m1),
            MailKind::Message,
            "second for m1".to_string(),
        )
        .unwrap();

    let first_visible = team.poll(MailEndpoint::Member(m1), 0)[0].id;
    let inbox = team.poll(MailEndpoint::Member(m1), first_visible);

    assert_eq!(
        inbox.iter().map(|mail| mail.id).collect::<Vec<_>>(),
        vec![broadcast, newer_direct]
    );
    assert!(
        inbox
            .iter()
            .all(|mail| mail.to == MailEndpoint::Member(m1) || mail.to == MailEndpoint::Broadcast)
    );
}

#[test]
fn status_is_recipient_scoped_and_counts_only_open_tasks() {
    let m1 = MemberId::new();
    let m2 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1, m2]);

    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Member(m1),
        MailKind::Message,
        "m1 only".to_string(),
    )
    .unwrap();
    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Member(m1),
        MailKind::Message,
        "m1 follow-up".to_string(),
    )
    .unwrap();
    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Member(m2),
        MailKind::Message,
        "m2 only".to_string(),
    )
    .unwrap();
    team.send_message(
        MailEndpoint::Lead,
        MailEndpoint::Broadcast,
        MailKind::Announcement,
        "shared".to_string(),
    )
    .unwrap();

    let open = team.task_create("keep open".to_string(), "details".to_string());
    let in_progress = team.task_create("in progress".to_string(), "details".to_string());
    let done = team.task_create("done".to_string(), "details".to_string());

    team.task_update(open, TaskStatus::Open, Some(m1)).unwrap();
    team.task_update(in_progress, TaskStatus::InProgress, Some(m1))
        .unwrap();
    team.task_update(done, TaskStatus::Completed, Some(m2))
        .unwrap();

    let m1_status: TeamStatusSnapshot = team.status(MailEndpoint::Member(m1));
    let m2_status: TeamStatusSnapshot = team.status(MailEndpoint::Member(m2));

    assert_eq!(m1_status.unread, 3);
    assert_eq!(m2_status.unread, 2);
    assert_eq!(m1_status.open_tasks, 1);
    assert_eq!(m2_status.open_tasks, 1);
}

#[test]
fn shutdown_reject_restores_active_state_without_marking_members_done() {
    let m1 = MemberId::new();
    let m2 = MemberId::new();
    let mut team = TeamControlPlane::new(&[m1, m2]);

    team.mark_member(m1, MemberState::Active);
    team.mark_member(m2, MemberState::Active);
    team.shutdown_request().unwrap();
    team.reject_shutdown().unwrap();

    let status = team.status(MailEndpoint::Lead);
    assert_eq!(status.state, TeamState::Active);
    assert_eq!(status.members.len(), 2);
    assert!(
        status
            .members
            .iter()
            .all(|(_, member_state)| *member_state == MemberState::Active)
    );
    assert!(
        status
            .members
            .iter()
            .all(|(_, member_state)| *member_state != MemberState::Done)
    );
}

#[test]
fn approve_shutdown_allows_closure_ready_and_failed_peers() {
    let active = MemberId::new();
    let closure_ready = MemberId::new();
    let failed = MemberId::new();
    let mut team = TeamControlPlane::new(&[active, closure_ready, failed]);

    team.mark_member(active, MemberState::Active);
    team.mark_member(closure_ready, MemberState::Active);
    team.mark_member(failed, MemberState::Active);
    team.shutdown_request().unwrap();

    team.mark_member(closure_ready, MemberState::ClosureReady);
    team.mark_member(failed, MemberState::Failed);
    team.approve_shutdown(active).unwrap();

    assert_eq!(team.state(), TeamState::Completed);
}
