use hya_proto::{ActorClaim, RosterStatus, SessionId, scope};
use hya_tool::extension::{ChannelProjection, ExtensionEvent, TeamProjection, team_projection};

use super::ChannelHistoryRead;
use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// `list_channel` rows for the acting agent (ADR-0016): the group channels
    /// it belongs to (home + led) with speak rights, and DM channels whose peer
    /// is LIVE (archived peers surface via `search_agent`, not here).
    pub(crate) async fn team_channel_rows(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::ChannelRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection_shared(root).await?;
        let team = team_projection(&projection);
        let cursor = team
            .roster
            .get(&handle)
            .map_or(0, |entry| entry.resident_cursor);
        let unread_indices: std::collections::HashSet<_> = team
            .inboxes
            .get(&handle)
            .into_iter()
            .flatten()
            .skip(usize::try_from(cursor).unwrap_or(usize::MAX))
            .copied()
            .filter(|&index| team.mail[index].from != handle)
            .collect();
        let mut rows = Vec::new();
        for (id, channel) in &team.channels {
            let member = channel.members.contains(&handle);
            if !member {
                continue;
            }
            let unread = channel
                .log
                .iter()
                .filter(|index| unread_indices.contains(*index))
                .count();
            match channel.kind {
                hya_proto::ChannelKind::Group => rows.push(hya_tool::ChannelRow {
                    id: id.clone(),
                    kind: hya_proto::ChannelKind::Group,
                    can_speak: channel.unit.as_deref() == Some(handle.as_str()),
                    peer: None,
                    unread,
                }),
                hya_proto::ChannelKind::Dm => {
                    // Peer identity from membership; hide archived peers.
                    let peer = channel
                        .members
                        .iter()
                        .find(|member| *member != &handle)
                        .cloned();
                    let peer_live = peer
                        .as_ref()
                        .is_some_and(|peer| team.roster.contains_key(peer));
                    if peer_live {
                        rows.push(hya_tool::ChannelRow {
                            id: id.clone(),
                            kind: hya_proto::ChannelKind::Dm,
                            can_speak: true,
                            peer,
                            unread,
                        });
                    }
                }
            }
        }
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(rows)
    }

    /// Read one channel's recent history for the acting agent (ADR-0016
    /// `channel://` read): newest-last, at most `last` messages (1 when
    /// `None`). Reading marks the caller's whole current inbox as seen (a
    /// single durable cursor cannot selectively consume one channel), and the
    /// result reports how many unread remain so the model knows to iterate
    /// `list_channel`.
    pub async fn read_channel_history(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        channel: &str,
        last: Option<usize>,
    ) -> Result<ChannelHistoryRead, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        // Taken before the snapshot below: work counted later stays owed.
        let mark = self.mail_mark(session);
        let projection = self.read_projection_shared(root).await?;
        // Models write `##X`, `#X`, or whitespace-padded ids when reading mail
        // history. Normalize (trim + strip every leading `#`; case stays
        // exact) and echo the correction so the right spelling is learned
        // in-turn instead of failing the read.
        let normalized = channel.trim().trim_start_matches('#');
        let mut warning = (normalized != channel)
            .then(|| format!("normalized channel id `{channel}` → `{normalized}`"));
        // A real channel id always wins. Channel ids are minted `DM-…` /
        // `announce-…` keys or qualified `<unit>#<name>` keys, never a bare
        // member path, so a handle alias cannot shadow one.
        let team = team_projection(&projection);
        let (normalized, channel_state) = match team.channels.get(normalized) {
            Some(state) if state.members.contains(&handle) => (normalized.to_string(), state.clone()),
            // Unknown and not-a-member are deliberately indistinguishable.
            Some(_) => return Err(unknown_channel(&team, &handle, normalized)),
            None => {
                // A member handle (canonical, or a leaf) names the caller's
                // DM with that member.
                let Some((id, state)) = dm_with_handle(&team, &handle, normalized) else {
                    return Err(unknown_channel(&team, &handle, normalized));
                };
                let alias = format!(
                    "`#{normalized}` is a member handle, not a channel: showing your DM with it, `{id}` (read it as `read channel://{id}`)"
                );
                warning = Some(match warning {
                    Some(earlier) => format!("{earlier}; {alias}"),
                    None => alias,
                });
                (id, state)
            }
        };
        let last_n = last.unwrap_or(1).clamp(1, 50);
        // Newest-first: the most recent message leads, matching how a chat
        // backlog is scanned.
        let messages: Vec<(String, String)> = channel_state
            .log
            .iter()
            .rev()
            .take(last_n)
            .map(|index| {
                let message = &team.mail[*index];
                (message.from.clone(), message.body.clone())
            })
            .collect();
        let inbox = team.inboxes.get(&handle);
        let inbox_len = inbox.map_or(0, |inbox| inbox.len() as u64);
        let cursor = team
            .roster
            .get(&handle)
            .map_or(0, |entry| entry.resident_cursor);
        // Whether this read showed every unread message: only then has the
        // caller seen what the cursor commit below consumes.
        let mut unread = team.unread_incoming_mail(&handle, cursor).peekable();
        let incoming = unread.peek().is_some();
        let showed_all_unread = unread.all(|unread| {
            messages
                .iter()
                .any(|(from, body)| *from == unread.from && *body == unread.body)
        });
        drop(unread);
        drop(projection);
        if cursor < inbox_len {
            self.emit_for_actor(
                actor_claim,
                root,
                ExtensionEvent::MailConsumed {
                    session: root,
                    handle: handle.clone(),
                    through: inbox_len,
                }
                .into_event(),
            )
            .await?;
            // Explicit reads consume mail before the next automatic steer
            // drain. Acknowledge that delivery to the supervisor as well,
            // otherwise it schedules a synthesis for results already seen —
            // but only when the read showed all of it: unread mail on other
            // channels (or past `last`) keeps its wake and synthesis.
            if showed_all_unread {
                self.notify_mail_consumed(session, mark, incoming);
            }
        }
        Ok(ChannelHistoryRead {
            channel: normalized,
            messages,
            unread_remaining: 0,
            warning,
        })
    }

    /// Direct-child liveness rows for the acting agent (ADR-0002): each roster
    /// entry whose parent is the caller's canonical handle, with its live
    /// status and harness-heartbeat freshness. Seconds-since-activity is
    /// computed here because the tool plane has no clock; `None` means no
    /// heartbeat was ever observed for that child.
    pub async fn team_member_status(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::MemberStatusRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection_shared(root).await?;
        let now = u64::try_from(hya_proto::now_millis()).unwrap_or(0);
        let mut rows = Vec::new();
        for (path, entry) in &team_projection(&projection).roster {
            if scope::parent_path(path) != Some(handle.as_str()) {
                continue;
            }
            rows.push(hya_tool::MemberStatusRow {
                handle: path.clone(),
                status: roster_status_label(entry.status).to_string(),
                last_active_seconds: (entry.heartbeat_ms > 0)
                    .then(|| now.saturating_sub(entry.heartbeat_ms) / 1_000),
            });
        }
        rows.sort_by(|left, right| left.handle.cmp(&right.handle));
        Ok(rows)
    }
}

/// Stable lowercase label for one live roster status row.
fn roster_status_label(status: RosterStatus) -> &'static str {
    match status {
        RosterStatus::Idle => "idle",
        RosterStatus::Busy => "busy",
        RosterStatus::Done => "done",
        RosterStatus::Failed => "failed",
    }
}

/// The caller's DM channel with the member `raw` names (canonical handle, or
/// a leaf resolved team-wide or under the caller), if one exists.
fn dm_with_handle(
    team: &TeamProjection,
    caller: &str,
    raw: &str,
) -> Option<(String, ChannelProjection)> {
    let id = team.resolve_dm(caller, raw)?;
    let state = team.channels.get(&id)?.clone();
    Some((id, state))
}

/// "Unknown channel" for a `channel://` read, naming the caller's own
/// channels (never anyone else's) and `list_channel`.
fn unknown_channel(team: &TeamProjection, caller: &str, raw: &str) -> CoreError {
    const LISTED: usize = 8;
    let mut mine: Vec<String> = team
        .channels
        .iter()
        .filter(|(_, channel)| channel.members.iter().any(|member| member == caller))
        .map(|(id, channel)| {
            let peer = (channel.kind == hya_proto::ChannelKind::Dm)
                .then(|| channel.members.iter().find(|member| *member != caller))
                .flatten();
            match peer {
                Some(peer) => format!("`{id}` (DM with {peer})"),
                None => format!("`{id}`"),
            }
        })
        .collect();
    mine.sort();
    let more = mine.len().saturating_sub(LISTED);
    mine.truncate(LISTED);
    let yours = if mine.is_empty() {
        "you have no channels".to_string()
    } else if more > 0 {
        format!("your channels: {} and {more} more", mine.join(", "))
    } else {
        format!("your channels: {}", mine.join(", "))
    };
    CoreError::Invalid(format!(
        "unknown channel `#{raw}`; {yours}. Read one with `read channel://<id>`; `list_channel` lists them with unread counts."
    ))
}
