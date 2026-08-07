# Batch H - server-client.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/architecture/server-client.md`

You have **6 gap entries** and **0 stale claims** to resolve.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 6 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/server-client.md`

**1. [api] HTTP permission API** — `thin` · severity medium

- Source: `crates/hya-server/src/compat/permission.rs:13-29`
- Evidence: docs/architecture/server-client.md:92 gives one route-group row with globbed paths (`/permission`, `/api/permission/*`, "session-scoped pending queues"). The concrete route list, the reply vocabulary and the feedback field are documented nowhere.
- Write: Expand the Permissions row into a dedicated subsection listing the routes: GET /permission; POST /permission/:request/reply; GET /api/permission/request; GET /api/permission/saved; DELETE /api/permission/saved/:id; GET /api/session/:id/permission; POST /api/session/:id/permission/:request/reply; and the legacy POST /session/:id/permissions/:request. Document the reply body vocabulary: `once` | `always` | `reject`, with an optional feedback message on reject that is surfaced to the model inside the denial error.

**2. GET/PATCH /config and /global/config are a runtime-only in-memory bag, not config.yaml** — `thin` · severity medium

- Source: `crates/hya-server/src/compat/catalog.rs:24-27, 54-70; crates/hya-server/src/compat/global.rs:14-63`
- Evidence: docs/compat-parity.md:113 says only "Global config is runtime-only" inside a long parity paragraph, and docs/architecture/server-client.md's route sections do not cover /config at all. Nothing warns that PATCH /config replaces the WHOLE object and is never persisted to config.yaml.
- Write: In the Compat-Compatible Route Groups section of docs/architecture/server-client.md, add an explicit note on `/config` and `/global/config`: these expose a runtime, in-memory key/value bag that starts as `{}` and is NOT backed by, loaded from, or written to config.yaml (catalog.rs:24-27,54-70; global.rs:14-63). PATCH REPLACES the entire object rather than merging, the only validation performed is that `username`, when present, must be a string, and all state is lost on restart. Cross-link this from the MCP Servers section of docs/configuration.md, which already makes the analogous "these routes do not durably rewrite config.yaml" point.

**3. Provider catalog HTTP endpoints (`GET /api/provider`, `/api/provider/:id`, `/api/model`, legacy `/config/providers`, `/provider`, `/provider/auth`) and Engine::provider_catalog** — `thin` · severity medium

- Source: `crates/hya-server/src/compat/catalog.rs:39, :40, :41, :28, crates/hya-core/src/engine.rs:409`
- Evidence: docs/architecture/server-client.md:91 lists the route paths in a single table row ("resolved hya provider catalog and local auth token store") with no response shape. docs/compat-parity.md:89/:114 describes coverage in prose. Nothing states which fields each endpoint returns or the 404 body shape.
- Write: Expand the Provider/auth row into a short subsection under `## Compat-Compatible Route Groups`. Say all of these are derived from the live router catalog, which the engine exposes via `Engine::provider_catalog` (engine.rs:409). `GET /api/provider` lists provider info; `GET /api/provider/:provider_id` returns one provider or a 404 with a `ProviderNotFoundError` payload when the id is not in the catalog (catalog.rs:40). `GET /api/model` lists every catalog model with tool support (derived from `streaming_tool_calls`), context window (from `max_context`) and the reasoning variant list (catalog.rs:41). The legacy trio `GET /config/providers`, `GET /provider`, `GET /provider/auth` expose the same catalog in the legacy shape, with auth always reported as a single `api` method regardless of whether the route actually uses OAuth (catalog.rs:28). Note the fallback: when the router catalog is empty the server synthesizes one entry from the active agent model with no variants (catalog.rs:194).

**4. WorkspaceAdapterInfo wire shape** — `thin` · severity low

- Source: `crates/hya-proto/src/workspace.rs:4`
- Evidence: docs/compat-parity.md:120 says '/experimental/workspace/adapter returns registered plugin workspace adapter metadata' but never gives the shape. No rustdoc on the struct. docs/architecture/server-client.md does not cover it.
- Write: In the Compat route-group section, document the WorkspaceAdapterInfo body returned by GET /experimental/workspace/adapter as {type, name, description}, and note it is populated by plugin-provided workspace adapters registered on AppState.

**5. CommandRequest and ShellRequest bodies** — `thin` · severity medium

- Source: `crates/hya-proto/src/api.rs:26`
- Evidence: docs/architecture/server-client.md:33-34 lists the routes and names the DTO types in a table, and :67-68 says 'command records command metadata before running a turn' — but unlike CreateSessionRequest (which gets a full JSON example at :48-63) neither body's fields are given. api.rs has zero rustdoc.
- Write: Add JSON bodies next to the existing CreateSessionRequest example: CommandRequest = {command, arguments, text?} and state the synthesis rule — when `text` is absent the server builds `/<command> <arguments>` as the admitted user message. ShellRequest = {command}. Also give PromptRequest = {text} and PromptResponse = {message, finish} explicitly rather than describing them in prose.

**6. ApiError status mapping (404 not_found and 503 service_unavailable)** — `thin` · severity low

- Source: `crates/hya-server/src/lib.rs:54`
- Evidence: docs/architecture/server-client.md:38-42 covers 400 (unparseable session id), 409 (busy) and 500 (runtime errors) but omits the not_found(404) and service_unavailable(503) constructors that also exist on ApiError.
- Write: Complete the status-code list in the Native Routes section: 400 bad_request for an unparseable session id, 404 not_found for a missing session, 409 conflict for a busy run, 503 service_unavailable, and 500 internal for every other CoreError/StoreError.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 6 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
