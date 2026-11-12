# Why must a revive boundary bypass user-text hooks?

The prior-handoff message is System context whose leading
`HYA_COMPACTED_CONTEXT` marks the episode cut. A user hook that prepends text
removes that prefix and exposes previous episodes to request construction.
`EpisodeInit.initial_prompt` names only the newly registered User directive;
revival passes None. Reviving mail is hooked later by ordinary work admission.

# Does revival create another member?

No. `agent.spawn` is a creation callback, guarded by the initializer's
`SessionCreated` event. Session hooks can be reattached on revival without
calling the parent's creation hook again.

# Where does a promptless catalog agent get its base after revival?

The engine retains the host base prompt supplied by app composition through
`with_base_system_prompt`. Revival and autonomous main reconstruction use that
same base; a catalog prompt replaces it only when present. Do not infer the
host base from the resolved root agent, whose own catalog prompt may differ.

An unchanged captured hook chain does not receive another `session.start`:
`replace_session_hooks` only starts fresh members. To hold a revived worker
before its first wake in a test, hold a real turn lease rather than gating an
assumed creation or session-start callback.
