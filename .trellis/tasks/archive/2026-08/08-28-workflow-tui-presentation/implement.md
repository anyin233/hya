# Implementation Plan

1. Add pure red tests for parsing typed Workflow Session state and presenting none, ready, running fan-out, completed, failed, cancelled, interrupted, stale, and unavailable states.
2. Implement hya-owned Workflow types/presentation helpers with exhaustive status handling and bounded text.
3. Add red plugin tests for `sidebar_content` registration/order/cleanup, semantic colors, clipping, Stage progress, and `first +N`. Implement the built-in sidebar view using existing plugin slots.
4. Add red Sync tests for bootstrap hydration and one `session.updated` replacement. Assert no timers, repeated fetches, or second SDK client; extend existing typed parse/state only.
5. Add a mutation test with unrelated run-tree members. Keep counts/activity server-derived from Workflow Member references.
6. Verify existing `/workflow` server command completion/submission and transcript behavior. Do not add a local slash parser or prompt special case.
7. Extend PTY tests at the repository's narrow/wide sizes for sidebar visibility, selection, running/terminal state, restored state, and existing roster navigation.
8. Run the two TUI mutations, revert them, then run focused typecheck/tests.
9. Build and drive the actual backend/TUI in a PTY. Observe simple Workflow selection, fan-out count/Stage text, completion/failure, restart restoration, and narrow layout.
10. Run Trellis check review and finish with recorded visual evidence.

Rollback: all TUI work is additive behind one built-in registration. If the server contract is incomplete, do not add a local fallback state or poller; leave this child blocked on the typed state dependency.
