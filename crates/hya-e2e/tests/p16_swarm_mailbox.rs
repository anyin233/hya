//! T2.4–T2.6, T2.9–T2.11 — the native swarm tools (`roster`, `list_agents`,
//! `send`, `channels`, `join`, `leave`) exercised against the real backend.
//!
//! Why these scenarios look different from every other Track P file:
//!
//! 1. **Mail only reaches a resident.** `hya-core::resident` injects a handle's
//!    unread inbox into that agent's next turn as a `[mail from …] …` user
//!    prompt. A transient subagent runs one turn and dies, so it can never
//!    observe delivery. Every scenario here therefore spawns `resident: true`
//!    teammates via the `task` tool.
//! 2. **The mailbox has no HTTP route.** There is no `/session/{id}/mailbox`, so
//!    a test cannot read delivery state the way `p15` reads `/session/{id}/todo`.
//!    The only honest proof that a message was *delivered* rather than merely
//!    *emitted* is that it shows up in the **recipient's own** next model
//!    request. That is what `route_dump` records.
//! 3. **The acting agents are residents, not the root.** Once a team has
//!    residents the root is registered as main-as-actor, and the supervisor wakes
//!    it on quiescence — an extra, unscheduled turn on the root session. Driving
//!    the swarm tools from the root would race those synthesis turns for the
//!    shared script queue. Scripting them on routed residents instead makes the
//!    ordering deterministic and leaves the root queue empty, so a synthesis turn
//!    is a harmless no-op.
//!
//! Two ordering rules make the multi-agent choreography deterministic without
//! sleeps:
//!
//! - **A resident may only address teammates spawned before it**, because
//!   `spawn_resident` registers members one at a time and each starts its initial
//!   turn as soon as it is registered. The agent that starts a chain is therefore
//!   always the last member of the `task` batch.
//! - **Work is chained by direct mail, one hop at a time.** Fanning out to two
//!   teammates at once would let their replies coalesce into one wake or arrive
//!   as two, which changes how many steps the receiver pops off its route.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnv, E2eEnvBuilder, ScriptStep, text_step, tool_step, tools_step};
use serde_json::{Value, json};

/// Route markers. Each is a prefix of the teammate's whole system prompt, which
/// is the only part of a request that belongs solely to the agent being asked —
/// the root echoes these strings back in its own `task` tool-call arguments, so
/// matching on the full body would let the root steal its teammates' queues.
const SYS_A: &str = "SYS_MARKER_A";
const SYS_B: &str = "SYS_MARKER_B";
const SYS_C: &str = "SYS_MARKER_C";

/// `ResidentSupervisor::assign_handle` names members `{agent_type}-{n}` within
/// their unit in spawn order, so position in the `task` batch fixes the leaf.
///
/// These three are all spawned by the root, so they are peers in one unit and
/// address each other by leaf. Their canonical paths (below) are what the roster
/// and every delivery receipt are keyed by.
const H1: &str = "general-1";
const H2: &str = "general-2";
const H3: &str = "general-3";

/// Canonical paths of the same three agents (task 08-07): the roster is keyed by
/// path, while `send` still accepts the relative leaf a teammate would type.
const P1: &str = "main/general-1";
const P2: &str = "main/general-2";

const TIMEOUT: Duration = Duration::from_secs(20);

/// One `task` member spawning a resident whose system prompt starts with `marker`.
fn resident_member(marker: &str, directive: &str) -> Value {
    json!({
        "description": format!("resident {marker}"),
        "prompt": directive,
        "subagent_type": "general",
        "resident": true,
        "inline_agent": { "prompt": format!("{marker} You are a resident teammate.") }
    })
}

/// Spawn the listed residents in one `task` call, then finish the root turn.
///
/// The root queue is deliberately drained by the end of this turn: any later
/// quiescence-driven synthesis turn on the root then finds an empty queue and
/// stops cleanly instead of stealing a teammate's step.
fn root_spawn_scripts(members: Vec<Value>) -> Vec<ScriptStep> {
    vec![
        tool_step("task", json!({ "members": members })),
        text_step("ROOT_SPAWNED"),
    ]
}

/// Drive the root turn that spawns the team, then wait until every teammate has
/// been asked at least once — i.e. the residents are really running turn loops,
/// not merely sitting in the roster.
async fn start_team(env: &E2eEnv, markers: &[&str]) -> hya_proto::SessionId {
    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "spawn the team")
        .await
        .expect("spawn prompt");
    for marker in markers {
        env.wait_route_requests(marker, 1, TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{marker} never ran a turn: {e}; {}", env.diagnostics()));
    }
    session
}

/// Roster entries carried on the run tree (`children[].roster`), keyed by handle.
fn tree_roster_entry(tree: &Value, handle: &str) -> Option<Value> {
    tree.get("children")?.as_array()?.iter().find_map(|child| {
        let roster = child.get("roster")?;
        (roster.get("handle")?.as_str()? == handle).then(|| roster.clone())
    })
}

/// Wait for `needle` in `marker`'s recorded requests, panicking with every
/// teammate's dump so a failure says which agent stalled and where.
async fn expect_route_sees(env: &E2eEnv, markers: &[&str], marker: &str, needle: &str) {
    if let Err(error) = env.wait_route_contains(marker, needle, TIMEOUT).await {
        let dumps = markers
            .iter()
            .map(|m| format!("\n--- {m} ---\n{}", env.route_dump(m).unwrap_or_default()))
            .collect::<String>();
        panic!(
            "{marker} never saw {needle}: {error}; {}{dumps}",
            env.diagnostics()
        );
    }
}

/// T2.4 — `roster` reports the live team; `list_agents` reports the spawnable
/// agent definitions visible to a resident subagent.
///
/// Oracle: agent A's own **follow-up** request — the turn after its tool calls,
/// so the transcript carries the tool *results*, not the arguments A sent.
#[tokio::test]
async fn t2_4_roster_and_list_agents_report_live_team_from_a_resident() {
    // Spawn order: B first (observed), A last (observer + chain starter).
    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B stands by"),
            resident_member(SYS_A, "A inspects the team"),
        ]))
        .route(
            SYS_A,
            vec![
                tools_step(vec![("roster", json!({})), ("list_agents", json!({}))]),
                text_step("A_DONE"),
            ],
        )
        .route(SYS_B, vec![text_step("B_IDLE")])
        .build()
        .await
        .expect("e2e env");

    let session = start_team(&env, &[SYS_A, SYS_B]).await;

    env.wait_route_requests(SYS_A, 2, TIMEOUT)
        .await
        .unwrap_or_else(|e| panic!("A never saw its tool results: {e}; {}", env.diagnostics()));
    let a = env.route_dump(SYS_A).expect("A dump");

    let tree = env.session_tree(&session).await.expect("tree");
    let b_entry = tree_roster_entry(&tree, P1)
        .unwrap_or_else(|| panic!("{P1} missing from run tree; tree={tree}"));
    let b_session = b_entry
        .get("session")
        .and_then(Value::as_str)
        .expect("session");

    // The roster result must carry the teammate's handle, agent type, live
    // status, and its real session id. The session id is the load-bearing one:
    // A never supplied it, so it can only come from projected team state.
    for needle in [
        P1,
        "\\\"type\\\":\\\"general\\\"",
        "\\\"status\\\"",
        b_session,
    ] {
        assert!(
            a.contains(needle),
            "A's follow-up request must carry the roster result containing {needle}; \
             b_session={b_session}; dump={a}; {}",
            env.diagnostics()
        );
    }
    assert!(
        a.contains(P2),
        "roster must also list the calling resident {P2}; dump={a}; {}",
        env.diagnostics()
    );
    // `list_agents` is agent *definitions*, not the live roster: a resident must
    // still see the spawnable catalog.
    assert!(
        a.contains("agents available"),
        "A's follow-up must carry the list_agents result; dump={a}; {}",
        env.diagnostics()
    );

    assert_eq!(
        env.fake.route_remaining(SYS_A).expect("remaining"),
        Some(0),
        "A must have consumed its own queue; {}",
        env.diagnostics()
    );
}

/// T2.5 — `send` to a teammate handle is *delivered*, not merely emitted.
///
/// Oracle: the body appears in **B's** own next model request, as the
/// `[mail from general-2] …` prompt the resident supervisor injects. A's
/// tool-call arguments and its `Delivered from …` success string are ignored on
/// purpose — both are produced even when nothing reaches the recipient (a
/// self-addressed send reports `Delivered from general-2 to general-2
/// (1 recipient)` while B gets nothing).
#[tokio::test]
async fn t2_5_direct_send_is_delivered_into_the_recipients_next_turn() {
    const BODY: &str = "DIRECT_MAIL_MARKER_A1";

    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B stands by for mail"),
            resident_member(SYS_A, "A mails a teammate"),
        ]))
        .route(
            SYS_A,
            vec![
                tool_step("send", json!({ "to": H1, "body": BODY })),
                text_step("A_SENT"),
            ],
        )
        .route(SYS_B, vec![text_step("B_IDLE"), text_step("B_GOT_MAIL")])
        .build()
        .await
        .expect("e2e env");

    let _session = start_team(&env, &[SYS_A, SYS_B]).await;

    expect_route_sees(&env, &[SYS_A, SYS_B], SYS_B, BODY).await;

    let b = env.route_dump(SYS_B).expect("B dump");
    assert!(
        b.contains(&format!("[mail from {P2}] {BODY}")),
        "delivery must arrive attributed to the sender's handle; dump={b}; {}",
        env.diagnostics()
    );
}

/// T2.6 — `send` to a `#channel` reaches every current subscriber, and the
/// receipt's `recipients` count reflects that real membership.
///
/// Chain: A pings B → B joins `#squad` and acks → A posts to `#squad` → B is
/// woken with the channel post.
#[tokio::test]
async fn t2_6_channel_send_reaches_subscribers_and_reports_real_recipients() {
    const BODY: &str = "CHANNEL_MAIL_MARKER_C1";

    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B subscribes on request"),
            resident_member(SYS_A, "A drives the channel"),
        ]))
        .route(
            SYS_A,
            vec![
                tool_step("send", json!({ "to": H1, "body": "JOIN_SQUAD" })),
                text_step("A_ASKED"),
                tool_step("send", json!({ "to": "#squad", "body": BODY })),
                text_step("A_POSTED"),
            ],
        )
        .route(
            SYS_B,
            vec![
                text_step("B_IDLE"),
                tools_step(vec![
                    ("join", json!({ "channel": "squad" })),
                    ("send", json!({ "to": H2, "body": "B_READY" })),
                ]),
                text_step("B_JOINED"),
                text_step("B_GOT_CHANNEL_MAIL"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let _session = start_team(&env, &[SYS_A, SYS_B]).await;

    // Recipient-side proof: the post landed in the subscriber's own transcript.
    expect_route_sees(&env, &[SYS_A, SYS_B], SYS_B, BODY).await;
    let b = env.route_dump(SYS_B).expect("B dump");
    assert!(
        b.contains(&format!("[mail from {P2}] {BODY}")),
        "channel post must arrive attributed to the sender's handle; dump={b}; {}",
        env.diagnostics()
    );

    // And the receipt A saw counted exactly the one live subscriber. The needle
    // must name the *channel*: a direct `send` hard-codes `recipients: 1`
    // (`Engine::mail_send_for_actor`) regardless of delivery, so bare
    // `recipients:1` would already be satisfied by A's earlier direct send to B
    // and would prove nothing about channel fan-out. Only the channel branch
    // counts real subscribers (`SessionStore::append_channel_mail` walks
    // `channels[squad].members`), and A never joined, so `to #squad
    // (1 recipient)` can only come from B's membership.
    let channel_receipt = format!("Delivered from {P2} to #squad (1 recipient).");
    expect_route_sees(&env, &[SYS_A, SYS_B], SYS_A, &channel_receipt).await;
}

/// T2.9 — `channels` lists the team's channels with real member and message
/// counts.
///
/// Chain: A pings B → B joins `#squad`, posts one seed message, and acks →
/// A calls `channels`.
#[tokio::test]
async fn t2_9_channels_lists_membership_and_message_counts() {
    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B subscribes on request"),
            resident_member(SYS_A, "A inspects channels"),
        ]))
        .route(
            SYS_A,
            vec![
                tool_step("send", json!({ "to": H1, "body": "JOIN_SQUAD" })),
                text_step("A_ASKED"),
                tool_step("channels", json!({})),
                text_step("A_LISTED"),
            ],
        )
        .route(
            SYS_B,
            vec![
                text_step("B_IDLE"),
                tools_step(vec![
                    ("join", json!({ "channel": "squad" })),
                    ("send", json!({ "to": "#squad", "body": "SEED_POST" })),
                    ("send", json!({ "to": H2, "body": "B_READY" })),
                ]),
                text_step("B_JOINED"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let _session = start_team(&env, &[SYS_A, SYS_B]).await;

    // The `channels` result reaches A on the turn *after* the call.
    expect_route_sees(&env, &[SYS_A, SYS_B], SYS_A, "#squad").await;
    let a = env.route_dump(SYS_A).expect("A dump");
    // A never named B, so B's handle here can only come from channel membership.
    for needle in [
        format!("\\\"members\\\":[\\\"{P1}\\\"]"),
        "\\\"messages\\\":1".to_string(),
    ] {
        assert!(
            a.contains(needle.as_str()),
            "channels result must report real membership/counts containing {needle}; \
             dump={a}; {}",
            env.diagnostics()
        );
    }
}

/// T2.10 — `join` changes who a channel `send` reaches.
///
/// A posts to `#squad` *before* B joins and again *after*. Only the second post
/// may appear in B's transcript, which is what makes this a membership test
/// rather than another channel-delivery test.
#[tokio::test]
async fn t2_10_join_changes_the_set_of_channel_recipients() {
    const PRE: &str = "PRE_JOIN_BODY_MARKER";
    const POST: &str = "POST_JOIN_BODY_MARKER";

    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B joins on request"),
            resident_member(SYS_A, "A posts before and after"),
        ]))
        .route(
            SYS_A,
            vec![
                tools_step(vec![
                    // Posted while #squad has no members at all.
                    ("send", json!({ "to": "#squad", "body": PRE })),
                    ("send", json!({ "to": H1, "body": "JOIN_NOW" })),
                ]),
                text_step("A_PRE_POSTED"),
                tool_step("send", json!({ "to": "#squad", "body": POST })),
                text_step("A_POST_POSTED"),
            ],
        )
        .route(
            SYS_B,
            vec![
                text_step("B_IDLE"),
                tools_step(vec![
                    ("join", json!({ "channel": "squad" })),
                    ("send", json!({ "to": H2, "body": "B_JOINED" })),
                ]),
                text_step("B_ACK"),
                text_step("B_GOT_POST_JOIN"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let _session = start_team(&env, &[SYS_A, SYS_B]).await;

    // Positive control first: the post-join message really arrived.
    expect_route_sees(&env, &[SYS_A, SYS_B], SYS_B, POST).await;

    // The pre-join post was appended *before* `JOIN_NOW`, and a wake injects the
    // whole unread inbox in sequence order. B having seen `JOIN_NOW` therefore
    // proves the pre-join post was never in B's inbox — not merely that it was
    // still in flight.
    let b = env.route_dump(SYS_B).expect("B dump");
    assert!(
        b.contains("JOIN_NOW"),
        "B must have taken its wake turn for the pre-join ordering argument to hold; \
         dump={b}; {}",
        env.diagnostics()
    );
    assert!(
        !b.contains(PRE),
        "a channel post sent before B joined must never reach B; dump={b}; {}",
        env.diagnostics()
    );
}

/// T2.11 — `leave` stops channel mail from reaching the departed member.
///
/// The negative claim is bounded by two positive controls in the same run, per
/// `design.md`:
///
/// 1. **C**, a still-subscribed member, receives the post — so the send itself
///    demonstrably delivered.
/// 2. **B**, the departed member, is directly pinged from the *same* tool batch,
///    immediately after the channel post — so the ping is strictly later in the
///    team log than the post. B seeing the ping therefore proves the post was not
///    merely still in flight.
///
/// Control 2 rests on a cursor invariant, not on both messages landing in one
/// wake (they usually do not — a mutation run that left B subscribed delivered
/// them in two consecutive turns). `ResidentSupervisor::run_one_turn` injects
/// *every* inbox entry from `cursor` to the snapshot length, in sequence order,
/// and only then advances `cursor` to that length; nothing else moves it
/// forward. A resident can therefore never be shown message N+1 without having
/// been shown an unread message N. `route_dump` is cumulative across all of B's
/// requests, so "B saw the ping" ⇒ "B would also have seen the post, had it ever
/// entered B's inbox".
///
/// Without those controls, "the message did not arrive" would only mean "the
/// test did not wait long enough".
#[tokio::test]
async fn t2_11_leave_stops_channel_mail_reaching_the_departed_member() {
    const AFTER_LEAVE: &str = "AFTER_LEAVE_BODY_MARKER";
    const PING: &str = "PING_AFTER_LEAVE_MARKER";

    // Spawn order fixes handles: C = general-1 (stays subscribed),
    // B = general-2 (leaves), A = general-3 (sender; spawned last so it may
    // address both).
    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_C, "C stays subscribed"),
            resident_member(SYS_B, "B leaves later"),
            resident_member(SYS_A, "A drives the chain"),
        ]))
        .route(
            SYS_A,
            vec![
                tool_step("send", json!({ "to": H2, "body": "JOIN_NOW" })),
                text_step("A_ASKED"),
                tools_step(vec![
                    ("send", json!({ "to": "#squad", "body": AFTER_LEAVE })),
                    ("send", json!({ "to": H2, "body": PING })),
                ]),
                text_step("A_DONE"),
            ],
        )
        .route(
            SYS_B,
            vec![
                text_step("B_IDLE"),
                tools_step(vec![
                    ("join", json!({ "channel": "squad" })),
                    ("send", json!({ "to": H1, "body": "YOUR_TURN" })),
                ]),
                text_step("B_JOINED"),
                tools_step(vec![
                    ("leave", json!({ "channel": "squad" })),
                    ("send", json!({ "to": H3, "body": "B_LEFT" })),
                ]),
                text_step("B_LEFT_ACK"),
                text_step("B_FINAL"),
            ],
        )
        .route(
            SYS_C,
            vec![
                text_step("C_IDLE"),
                tools_step(vec![
                    ("join", json!({ "channel": "squad" })),
                    ("send", json!({ "to": H2, "body": "BOTH_IN" })),
                ]),
                text_step("C_JOINED"),
                text_step("C_GOT_CHANNEL_MAIL"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let markers = [SYS_A, SYS_B, SYS_C];
    let _session = start_team(&env, &markers).await;

    // Control 1: the channel post reached the member who never left.
    expect_route_sees(&env, &markers, SYS_C, AFTER_LEAVE).await;
    // Control 2: B took a turn for mail appended *after* the channel post.
    expect_route_sees(&env, &markers, SYS_B, PING).await;

    let b = env.route_dump(SYS_B).expect("B dump");
    assert!(
        !b.contains(AFTER_LEAVE),
        "channel mail must not reach a member that left the channel; dump={b}; {}",
        env.diagnostics()
    );
}

/// T2.12 — an agent cannot reach another unit, and the payload never crosses
/// (task 08-07, AC1).
///
/// Two levels deep, so there are genuinely two units:
///
/// ```text
/// main
/// ├── general-1              (B — the root's unit)
/// └── general-2              (A — the root's unit, and a leader)
///     └── general-1          (C — A's unit; its own leaf, restarted per unit)
/// ```
///
/// C and B share no parent, so C may not address B. C tries anyway, using B's
/// full path so the address is unambiguous rather than a leaf that happens to
/// match nothing.
///
/// Two oracles, because a rejection alone would also be produced by a broken
/// send that silently delivered elsewhere:
///
/// 1. C's **own** follow-up request carries the refusal, so the tool really
///    failed rather than quietly succeeding; and
/// 2. B's dump never contains the payload — nothing crossed the boundary.
#[tokio::test]
async fn t2_12_cross_unit_send_is_refused_and_never_crosses() {
    const CROSS: &str = "CROSS_UNIT_MARKER_X1";
    const TO_PARENT: &str = "TO_PARENT_MARKER_X2";

    let env = E2eEnvBuilder::new()
        .scripts(root_spawn_scripts(vec![
            resident_member(SYS_B, "B stands by in the root's unit"),
            resident_member(SYS_A, "A leads its own unit"),
        ]))
        .route(
            SYS_A,
            vec![
                // A opens a unit of its own by spawning C.
                tool_step(
                    "task",
                    json!({ "members": [resident_member(SYS_C, "C probes the boundary")] }),
                ),
                text_step("A_SPAWNED"),
                text_step("A_GOT_CHILD_MAIL"),
            ],
        )
        .route(SYS_B, vec![text_step("B_IDLE")])
        .route(
            SYS_C,
            vec![
                // Out of scope: B is in the ROOT's unit, C is in A's.
                tool_step("send", json!({ "to": P1, "body": CROSS })),
                // In scope: C's own parent, addressed by its short name.
                tool_step("send", json!({ "to": H2, "body": TO_PARENT })),
                text_step("C_DONE"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let markers = [SYS_A, SYS_B, SYS_C];
    let _session = start_team(&env, &markers).await;

    // Control: the in-scope send to C's parent DID arrive, so C's turn really
    // ran and the refusal below is about scope, not about C never acting.
    expect_route_sees(&env, &markers, SYS_A, TO_PARENT).await;

    let c = env.route_dump(SYS_C).expect("C dump");
    assert!(
        c.contains("not a teammate you can message"),
        "C's follow-up must carry the scope refusal; dump={c}; {}",
        env.diagnostics()
    );

    let b = env.route_dump(SYS_B).expect("B dump");
    assert!(
        !b.contains(CROSS),
        "a cross-unit payload must never reach the other unit; dump={b}; {}",
        env.diagnostics()
    );
}
