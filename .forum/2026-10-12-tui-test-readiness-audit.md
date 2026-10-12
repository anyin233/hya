# Why can TUI CI pass once and then fail after an unrelated change?

PR 32 run 38276709013 passed 300/301 scenarios but read the old session ID
immediately after `/new` in the cross-client resume case. Run 38278896333 passed
without changing that test. Input delivery and a closed command menu do not
acknowledge completion of an asynchronous command.

Use `e2e/hya.ts`'s `createSession` helper, which records the previous selection
and waits for a different ID. Supply `previousId` for titled sessions, since
`/status` displays their titles. The old sessions-spec helper compared against
an optional undefined ID, which let any old ID satisfy its wait.

Readiness markers must identify the current action. The notification test used
one identical reply for three turns, letting the final negative assertion run
before its turn completed. The Project scope test waited for single letters
already present in normal UI text. Use unique replies followed by idle.

The fake model also retained timed-out and disconnected hangs in its pending
queue; `release()` could target a dead request. The timeout regressions fail
before the fix on both protocols; coverage also verifies cancellation followed
by a new release, and shutdown. Use one held response per concurrent child; an
exhausted route returns an empty answer immediately.

Wait for client catalog synchronization through command completion instead of
a guessed 600 ms delay after backend discovery. Completion reads the current
TUI catalog without refreshing it; repeat the query with an edit because its
suggestions are cached between edits.

The shared-database scenario issued `/status` once while startup could still
restore Conversation. Its reader now reopens and polls until daemon metadata
is drawn. The temporary-session case waits for the Project view to close after
`t` before opening `/status`, so it does not preempt the extension's key handler.

Local environment pitfall: symlinking SDK runtime dependencies outside its
directory makes the extension OS sandbox reject their reads. Use real runtime
dependencies inside the SDK when testing sandboxed extensions. The prepared
data-disk verification worktree has that setup. Compiler output, temporary
workspaces, and terminal captures stay under ~/data.

Rebuild `packages/hya-tui` after advancing a reused verification checkout:
`src/main.ts` prefers an existing `dist/app.js` over source, even if that build
is stale. An old build made the new archive cases fail with the backend
already showing `archived: true`; a fresh frontend build fixes that local
verification mismatch. All five follow-up provider/temporary-session/archive/
shared-database cases then passed (6.6 seconds).
