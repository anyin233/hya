# Are completed subagents archived, and why does the selector not say so?

Read-only inspection of the backend on 22103 and its event log on 2026-10-12
confirmed five review sessions with 15 successful subagent reports and 15
`agent_archived` events, all with reason `reported`. None had a later restart.
The parent transcripts had no `archive` calls: accepted child reports archive
automatically, as implemented by `archive_reported_agent` (ADR-0015).

Exact actor archival is folded into `TeamProjection.archived`, separate from
root chat `SessionInfo.archived`. Child sessions remain readable after actor
archival. v1 MemberInfo exposes terminal outcome but no archive state/reason;
AgentArchived/AgentRestarted are not mapped into memberUpdated. The selector
uses childStatus and therefore shows done/failed/cancelled without saying
whether the actor has left the live roster. Snapshot handles also come only
from the live roster, so archived members can lose their readable handles.

The implemented contract exposes optional `MemberInfo.archived` and
`archiveReason`, resolving handles from both live and archived root-team entries.
`memberLifecycleChanged` is a separate child-session-keyed event: domain archive
and restart markers have no parent-local member ID and live on the team root
even for nested children. It reaches replay, SSE, gRPC, and the lightweight global
stream, so clients viewing a nested parent can refresh its projection without
mixing sequence watermarks. SDK mirrors and TUI member folding retain outcome
and clear archival/reason on revival; archived transcripts stay selectable.
Do not
infer it from the child session's root-only archived flag or make the parent
issue redundant archive calls after report.
