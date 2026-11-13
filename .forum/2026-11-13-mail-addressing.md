# Does a handle create a DM or persist as a mail target?

No. M4-11 resolves a canonical handle or leaf by the caller's existing DM
membership at the engine send boundary. Missing/ambiguous DMs reject the request;
there is no channel creation or roster/path fallback. The same resolution works
for archived peers and lazy root registration. `^parent`, `^` and the internal
empty handle select the direct parent; public omission still uses the role default
(led group, otherwise parent DM). Store authorization and archived-child revival
then share the explicit channel path. Steer uses the same boundary; Workflow
arming already uses its mandatory parent DM.

# Why keep Handle and ChannelJoined on the wire?

I21 reserves Handle for harness notices. The common event append rejects every
other Handle target, including raw append callers. ChannelJoined has no production
producer; its reducer fold is removed, while the enum stays to avoid an unrelated
wire/generated-API change. Registration extends membership with ChannelCreated; revival still restores the
existing group membership through AgentRegistered. In-turn steering reads the persisted
channel directly; harness notices have no channel, rather than borrowing an
arbitrary DM from a peer lookup table.
SubagentMode::Transient now identifies only the episode-less lead; member fixtures
and the unused mode parser are removed. No old-log or database compatibility is
provided. Reducer version advances because ChannelJoined no longer changes state.

# Does a send receipt count the sender's DM history copy?

Yes: aliases now use the existing channel receipt (two eligible DM members),
matching replay fan-out. Only the call that actually revives reports one
revived recipient; a losing concurrent reviver returns the writer transaction's
ordinary live fan-out count. The registration-pause regression fixes the order
so both callers initially observe archive and the second caller wins revival. Incoming-only unread/wake rules still exclude the sender.
Report endings require the registered parent DM and have no handle fallback.

# How does recovery treat the sender's new channel history copy?

A handle send now creates the same outgoing history copy as an explicit DM send.
Recovery tests exposed raw inbox-length comparisons classifying that copy as
Queued, or queued_after an interrupted wake. Store recovery, recovered episode
installation and cancellation's mail-owed decisions now use incoming-mail
iteration. Raw cursor positions stay unchanged. A retained admitted prompt still
owns a wake even with no new incoming mail, preserving resume-once behaviour.
This changes mail accounting only; startup ordering and team ownership are not
changed by M4-11.

# Why did I5 time out after channel-only handle sends?

A handle send now leaves an outgoing copy in main's DM history. I5 sends mail
between main turns; the next steering snapshot therefore starts with that raw
cursor backlog. After its fresh task registers, steering cleans up the outgoing
copies and commits MailConsumed. The tool-result collector treated any cursor
acknowledgement as consumed work and called notify_mail_consumed. That advanced
last_synth_work_seq to the fresh registration's work_seq, suppressing its owed
TEAM QUIESCED synthesis. The fresh requests already carried NEW_TEAM_GUIDANCE_M4;
the assertion's timeout branch, rather than a wrong-guidance request, failed.

A minimal variation that retained cursor commits but withheld the work-consumed
notification for an empty notice made I5 pass. The final fix carries whether the
acknowledgement covers incoming mail, including policy-filtered incoming mail,
and notifies the supervisor only in that case. Outgoing-only cursor cleanup
cannot settle unrelated work. No ownership, routing, or startup redesign is
needed; I5 now distinguishes a missing synthesis from wrong guidance directly.
