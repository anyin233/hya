# Quality Guidelines

> Code quality standards for backend development.

---

## Overview

<!--
Document your project's quality standards here.

Questions to answer:
- What patterns are forbidden?
- What linting rules do you enforce?
- What are your testing requirements?
- What code review standards apply?
-->

(To be filled by the team)

---

## Forbidden Patterns

<!-- Patterns that should never be used and why -->

(To be filled by the team)

---

## Required Patterns

<!-- Patterns that must always be used -->

(To be filled by the team)

---

## Testing Requirements

<!-- What level of testing is expected -->

(To be filled by the team)

---

## Code Review Checklist

<!-- What reviewers should check -->

(To be filled by the team)

---

## Scenario: Non-blocking prompt admission routes

### 1. Scope / Trigger

- Trigger: any HTTP/API route that admits a prompt or shell turn and starts optional model-side work such as auto-title, summarization, compaction, or background metadata generation.
- Applies to Compat-compatible prompt routes, native prompt routes, and future admission-style routes that must acknowledge work before the provider stream completes.

### 2. Signatures

- Route shape: `POST /api/session/{session_id}/prompt` and equivalent prompt-admission endpoints.
- Core sequence: parse session ID, validate/load session, durably admit the user request, schedule optional follow-up work, return the admission response.

### 3. Contracts

- The route may await storage/projection work required to admit the request.
- The route must not await optional provider streams before responding.
- Auto-title and similar optional follow-up work must run in a background task or be driven by a separate worker path.
- The event log remains authoritative: background follow-up state changes must still write normal events such as `SessionTitled` through `SessionEngine` helpers.

### 4. Validation & Error Matrix

- Invalid session ID -> typed bad-request/not-found response before any background work starts.
- Admission/storage failure -> route returns the admission error and does not claim the prompt was accepted.
- Optional provider/title/summarizer hangs -> prompt route still returns after admission; only the optional follow-up remains pending.
- Optional follow-up failure -> do not fail the already-returned admission response; surface through logs/events only if an owning error path exists.

### 5. Good/Base/Bad Cases

- Good: a pending title provider cannot block `POST /api/session/{id}/prompt`; session context can still show the admitted unfinished assistant state.
- Base: title generation eventually writes `SessionTitled` after the prompt response when the provider completes.
- Bad: awaiting `auto_title_session(...)` or another optional provider call inside the prompt handler before sending the HTTP response.

### 6. Tests Required

- Add a route-level regression with a fake provider whose optional follow-up future never resolves; assert the prompt route returns within a bounded timeout.
- Assert list/context APIs still reflect durable admission state while unfinished assistant/provider work has no completion timestamp.
- For eventual background results, poll a condition with a bounded timeout instead of asserting immediate replacement.

### 7. Wrong vs Correct

#### Wrong

```rust
admit_prompt(&state, session, request).await?;
state.engine.auto_title_session(session, model).await?;
Ok(Json(response))
```

#### Correct

```rust
let response = admit_prompt(&state, session, request).await?;
let engine = state.engine.clone();
tokio::spawn(async move {
    let _ = engine.auto_title_session(session, model).await;
});
Ok(Json(response))
```

---

## Scenario: Session ID compatibility across routes and fixtures

### 1. Scope / Trigger

- Trigger: any API route, client URL builder, sync/projector path, TUI/control route, or test fixture that accepts or emits a session ID.
- Applies to Compat-compatible `sessionID` payload fields, native path parameters, experimental routes, sync replay/history routes, and test helpers that create sessions.

### 2. Contracts

- New sessions are identified by the server-returned `hysec_[A-Za-z0-9]{20}` string.
- Legacy `ses_<uuid-simple>` and raw UUIDs may be parsed only through the shared `SessionId` parser for compatibility.
- Route-local prefix checks such as `starts_with("ses")` are forbidden.
- Test fixtures must use the session ID returned by the API or `SessionId::to_string()`; they must not rebuild IDs with string formatting.
- Storage and replay code must use the shared storage/display contract instead of assuming UUID bytes.

### 3. Tests Required

- Creation tests assert the `hysec_` shape.
- Every route family that accepts a session ID should include at least one flow using the returned `hysec_` ID.
- Legacy parser coverage belongs in the shared ID/parser tests or explicit compatibility tests, not by rewriting new IDs into legacy-looking strings.

### 4. Wrong vs Correct

#### Wrong

```rust
if !payload.session_id.starts_with("ses") {
    return Err(ApiError::bad_request("invalid session id"));
}
let session = parse_session(&payload.session_id)?;
```

```rust
let session_id = format!("ses_{}", created_session.replace('-', ""));
```

#### Correct

```rust
let session = parse_session(&payload.session_id)?;
```

```rust
let session_id = created_session;
```

---

## Scenario: CLI session persistence through database-backed commands

### 1. Scope / Trigger

- Trigger: any `hya-backend` command that creates, mutates, replays, lists, or serves sessions while accepting a SQLite database path.
- Applies to headless `exec` / `run`, `sessions`, `tail-session`, `serve`, and future CLI commands that share the event-sourced session store.

### 2. Signatures

- Headless execution: `hya-backend --db <path> exec <prompt>` and `hya-backend --db <path> run <prompt>`.
- Listing: `hya-backend sessions --db <path>`.
- Server: `hya-backend serve --db <path> --bind <addr>`.
- Empty `--db ""` remains the in-memory store mode; a non-empty path is a persistent SQLite store.

### 3. Contracts

- A command that receives a non-empty database path and emits session events must open that exact SQLite store before constructing `SessionEngine`.
- Headless `exec` / `run` output may render a transcript or JSONL stream, but the same events must be replayable from `sessions --db <path>` after the process exits.
- `serve --db <path>` and headless commands share the same `SessionStore` contract: `hysec_` IDs, projection replay, and list filtering all come from the SQLite event log.
- In-memory execution is allowed only when the effective DB path is empty; do not silently fall back to memory when a path is supplied.

### 4. Validation & Error Matrix

- Missing parent directory or invalid SQLite path -> command returns the store-open error and does not claim session persistence.
- `exec --db <path>` succeeds -> a subsequent `sessions --db <path>` lists the emitted `hysec_` session.
- `exec --json --db <path>` succeeds -> JSONL envelopes and persisted DB replay describe the same session ID.
- `serve --db <path>` prompt flow succeeds -> `sessions --db <path>` can list the same non-empty session after the HTTP request.

### 5. Good/Base/Bad Cases

- Good: `hya-backend --db /tmp/hya.db exec "Say hello"` writes events to `/tmp/hya.db`, and `hya-backend sessions --db /tmp/hya.db` prints the resulting `hysec_...` row.
- Base: omitting `--db` uses in-memory execution and does not leave a durable session after process exit.
- Bad: `exec` constructs `SessionStore::connect_memory()` even though the top-level CLI parsed `--db <path>`.

### 6. Tests Required

- Add a CLI integration regression that runs `hya-backend --pure --db <tmp>/hya.db exec <prompt>` and then asserts `hya-backend sessions --pure --db <tmp>/hya.db` contains `hysec_`.
- Manual QA should run a rendered `exec`, a JSONL `exec --json`, and `sessions --db` against the same DB to prove both output modes persist.
- HTTP QA should run `serve --db`, create/prompt a session, then list the same DB through the CLI.

### 7. Wrong vs Correct

#### Wrong

```rust
let store = SessionStore::connect_memory().await?;
let (engine, ..) = build_session_engine(store, router, &model, mcp, plugins).await;
```

#### Correct

```rust
let store = open_store(db).await?;
let (engine, ..) = build_session_engine(store, router, &model, mcp, plugins).await;
```

---

## Scenario: GitHub Release Binary Workflow

### 1. Scope / Trigger

- Trigger: any change that publishes release binaries, creates GitHub Releases, or modifies the release changelog process.
- Applies to `.github/workflows/release.yml`, root `CHANGELOG.md`, `docs/changes/`, root `AGENTS.md` release rules, and release-related task artifacts.

### 2. Signatures

- Release tag: `vX.Y.Z`, where `X.Y.Z` must match Cargo's `hya` package version.
- Cargo command: `cargo build --release --locked --bin hya --target x86_64-unknown-linux-gnu`.
- Release archive: `hya-<version>-x86_64-unknown-linux-gnu.tar.gz`.
- Checksum file: `SHA256SUMS` generated beside the release archive.

### 3. Contracts

- Root `CHANGELOG.md` contains only the newest version's release notes.
- Historical changelogs live under `docs/changes/CHANGELOG_<version>.md`.
- The GitHub Release body is read verbatim from root `CHANGELOG.md`.
- Release workflow permissions are read-only by default; only the release publishing job may request `contents: write`.
- Build provenance attestations are generated for the archive and checksum.
- Third-party release actions are pinned to immutable commit SHAs.
- The publishing job uses the `release` environment so repository settings can require manual approval.

### 4. Validation & Error Matrix

- Missing `v` tag prefix -> fail before build.
- Tag version is not semver-shaped -> fail before build.
- Tag version differs from `cargo metadata` package version for `hya` -> fail before build.
- Missing or empty `CHANGELOG.md` -> fail before publishing.
- `CHANGELOG.md` first heading differs from the tag version -> fail before build.
- Build, archive, checksum, or packaged-binary smoke failure -> skip release publishing.
- Missing release assets -> fail `softprops/action-gh-release` with `fail_on_unmatched_files: true`.

### 5. Good/Base/Bad Cases

- Good: `v0.1.0`, `[workspace.package].version = "0.1.0"`, root `CHANGELOG.md` contains only `0.1.0` notes, archive and checksum pass smoke checks.
- Base: first release has no historical changelog; keep `docs/changes/.gitkeep` and root `CHANGELOG.md` for the current version.
- Bad: appending old release notes to root `CHANGELOG.md`; this publishes stale history as the GitHub Release body.

### 6. Tests Required

- Parse workflow YAML, run `actionlint`, and syntax-check every embedded shell `run` block.
- Run the tag/version/changelog validation logic with a representative tag.
- Run the release build command for the configured target.
- Package the built binary, verify `SHA256SUMS`, extract the archive, and run packaged `hya --version` plus `hya --help`.
- Confirm third-party actions are pinned to commit SHAs and release publication uses the `release` environment.

### 7. Wrong vs Correct

#### Wrong

```yaml
permissions: write-all
```

```markdown
# CHANGELOG

## 0.2.0
- New release.

## 0.1.0
- Old release.
```

#### Correct

```yaml
permissions:
  contents: read

jobs:
  release:
    permissions:
      contents: write
```

```markdown
# 0.2.0

- New release.
```
