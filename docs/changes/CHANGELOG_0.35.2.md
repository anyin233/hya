# 0.35.2

## Workflows

- User-authored workflow DAGs are now executable end to end. A workflow is one
  user-authored file (`<workdir>/.hya/workflows/*.yaml|*.md`) declaring stages,
  `needs:` edges, and an explicit join contract; hya still ships **zero**
  built-in workflows, and nothing hardcodes a plan→impl→review pipeline.
- `hya-backend workflow run <name> [--input key=value ...] [--json]` executes a
  discovered workflow in-process and prints (or serializes) the per-stage run
  report; exit status reflects validation/input and overall run outcome.
  Input pairs split on the first `=` so values may contain `=` signs, and any
  key that the workflow does not declare is rejected alongside missing ones
  before anything spawns.
- The new governed `workflow` tool lets an agent discover and launch a
  user-authored DAG mid-session: `action=list` summarizes files under the
  discovery roots, `action=run` executes one by name with per-run inputs. Runs
  reuse the task permission class scoped to `workflow:<name>` so existing
  ask/deny rules carry over.
- Every workflow level executes as ONE batch through the same pre-admitted team
  path as task batches (`pre_admit_team` / `run_pre_admitted_team`), so a
  hand-written DAG cannot bypass subagent depth, streaming-concurrency, or
  per-run budget caps; workflows whose declared stage count exceeds the budget
  are rejected before any member spawns. Loop stages (`mode: loop` +
  `verify`) iterate through the
  shared iteration driver with an independent verifier agent owning the stop
  decision.
- Stage members reuse the task path's target execution context: each
  worker/verifier resolves its own `can_spawn` roster, resource policy, and
  Bundle sidecar factory from its stage agent — never the caller's roster and
  never a missing sidecar — so a stage can only delegate targets its own agent
  is authorized to spawn (regression-tested end to end, including in-stage
  `task` rejection and loop/verifier sessions).

## Tools

- The `find` tool now scopes its optional `path` like every other file tool:
  relative paths resolve against the session working directory, while absolute
  paths or `..` traversals that escape it go through the same
  external-directory permission check as `glob`, `grep`, read, and edit, and
  fail with a permission error when denied. Omitting `path` still searches the
  whole working directory.

## Provider resilience

- Streamed requests now include an auth-recovery level for OAuth routes: when
  a response arrives with HTTP 401 or 403 before any stream exists, a route
  built with the new forced-refresh hook force-refreshes the credential once,
  re-resolves auth headers, and retries inside the existing three-attempt
  budget (at most one forced-refresh retry, never extended or looped). If no
  hook is configured, it fails, or the resolved token did not change, the
  original status error surfaces unchanged. This level stays strictly on the
  pre-stream side of the no-replay boundary, and `AuthExpired` remains
  non-retryable for router/engine failover since re-login is a human action.
  The app-level entry point keeps the scheduled-refresh safety rails: the
  single-flight lock plus a re-read past the lock skip the network refresh
  when another stream already rotated past the failing access token, so
  concurrent streams recovering from 401 perform exactly one network refresh
  and do not burn the rotated refresh token.
- HTTP streamed-completion requests now retry up to three times before response
  headers establish the event stream when the failure is a connection/transport
  error, HTTP 429, or HTTP 5xx. Retries use bounded exponential backoff with
  jitter, honor a bounded `Retry-After` delay, and bound non-success response
  body reads so diagnostics cannot stall the next attempt.
- Streamed requests now enforce a bounded response-header deadline (60 seconds
  per attempt). A route that never returns headers fails as a retryable
  transport error, so the existing three-attempt retry and router failover
  still run instead of hanging.
- Established SSE streams now end with an idle-stall error when no frame
  arrives within five minutes — counting from the response headers for the
  first event and resetting on every delivered frame between events. Per the
  no-replay boundary this mid-stream timeout surfaces once to the caller and
  is never retried or failed over.
- Provider errors now distinguish transport failures and HTTP status responses
  from protocol and authentication failures, so retry decisions are explicit
  instead of inferred from error strings.
- When multiple registered providers claim the same model, the router now moves
  to the next route after a retryable pre-stream failure. Non-retryable errors
  fail immediately, and errors received after an event stream is returned are
  never replayed, preventing duplicate streamed output or tool side effects.
- Configured `categories:` model chains now drive a second recovery level in
  the engine: when the preferred model fails before any stream exists with a
  retryable error (or no route claims it), the turn re-issues the same request
  against the next candidate in the category's ordered chain. A switch is
  logged via `tracing` (from/to model). Recovery is two-level — same-model
  route failover stays inside `hya-provider`, cross-model chain failover lives
  in `hya-core` — and both levels share one strict no-replay boundary: once an
  event stream is established, errors are delivered once and are never
  replayed or failed over onto another model.
