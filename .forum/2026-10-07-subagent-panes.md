# How do subagent panes avoid switching the parent's session or duplicating connections?

`subagents` is a selectable layout job and `subagent-viewer` is passive. The workspace store keeps only an ephemeral preview ID. A viewer's optional `PaneLeaf.session` pins that instance and persists through the normal version-4 layout preferences. Never call the main controller's `openSession` to preview a child.

`app/sessionViews.ts` reference-counts watches by server/root/child scope and uses an ephemeral `createAppStore` for the existing projection/overlay renderer. A nested AppContext renders the existing Transcript/MessageItem components with the child's data and isolated UI handles. Scrollboxes belong to instances. Two viewers of one child share the watch; last release aborts it, and stale reads/frames are ignored. Viewer target effects depend on a stable scope key, so sidebar polling does not reconnect the stream.

# What can an already-running child stream replay?

Durable events and completed text are replayable; transient deltas are not. A viewer joining after a text part's live start frame may not show that part until its completed projection is stored. Open a following viewer before spawning helpers for complete live rendering. A future backend contract exposing live-part snapshots would be needed to recover every missed in-progress byte. Do not invent a second durable transcript model in the frontend.

# What verified this feature?

Focused unit coverage checks descendant filtering, nested/completed children, pin persistence, shared watches, last-release cleanup, stale reads and resync replay. Browser specs exercise normal and narrow layouts, strict input ownership, draft retention, restart, multiple different children, and live streaming. The opt-in real gRPC process test also exercises shared viewer replay and live refresh.
