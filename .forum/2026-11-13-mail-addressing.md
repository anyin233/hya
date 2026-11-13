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
matching replay fan-out. Revival preserves the existing channel receipt of one
revived recipient. Incoming-only unread/wake rules still exclude the sender.
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

# What still blocks clean acceptance of M4-11?

The final eleven-package suite passes 2,443 tests but fails
`i5_post_kill_task_uses_fresh_team_without_waiting_for_other_endings` at
"new main reused the killed team's activation context". Its `guidance` boolean
also becomes false on a ten-second timeout, so that assertion does not establish
actual context reuse. The unchanged base passes the focused test and the full
lifecycle binary; this failure is not classified as pre-existing. M4-11 leaves
team/context ownership and listener/startup ordering to M4-12. Investigate the
missing-versus-wrong-guidance distinction before choosing a fix.
