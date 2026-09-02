# Fix Session Tool Reliability And Port Native Coding Tools

## Goal

Restore reliable coding-tool and subagent execution in active or resumed hya
Sessions. Replace hya's built-in Read, Edit, and Grep behavior with a complete
native Rust port of `pi-hashline-edit` 0.8.3, align the host-owned Write and Bash
contracts with pinned Oh My Pi behavior where portable to hya, and render the
resulting tools as titled, syntax-aware OpenTUI blocks. Preserve hya's
permission and event-sourced architecture.

## Confirmed Facts

- The captured hya 0.36.8 Session persisted 17 lowercase `read` requests, 17
  `tool_error` events, and no Read results. Every request contained
  `filePath`, `path`, `offset`, and `limit`; every error was typed `input` with
  the exact message ``input: duplicate field `path` ``.
- `crates/hya-tool/src/read.rs` advertises both path spellings but maps them to
  one Serde field through an alias. The failure therefore occurs before path
  resolution, permission checks, or I/O. Provider and engine layers retain the
  arguments without renaming them.
- The same Session repeatedly sent `task.inline_agent.description = ""` and
  received `UNSUPPORTED_INLINE_AGENT_FIELD: `description``. The Task schema
  advertises that nested field, but hya-app rejects every non-null value before
  admission.
- `pi-hashline-edit` 0.8.3 is pinned by npm `gitHead`
  `ba7db9943d0f58499b24c1f6bd64722580f772a5` and tarball SHA-1
  `8985f24c3493be375cc225a5522ed54de8daabc9`. It defines Read, Edit, and
  optional Grep. It does not define Write or Bash.
- Oh My Pi tool and presentation behavior is pinned to
  `can1357/oh-my-pi@0b769cc4dd9771373335430385d1d2f696dc3498`
  (`@oh-my-pi/pi-coding-agent` 18.1.3). Its Write and Bash contracts are host
  behavior, not hashline behavior.
- The TypeScript/OpenTUI frontend is the sole interactive renderer. Existing
  Write and Edit paths use `<code>` and `<diff>`, while Read and Grep are
  inline-only and canonical `shell` falls through to the generic renderer.

## Requirements

### Native And Architectural Boundaries

- Implement all built-in coding-tool execution in Rust. Do not add an npm,
  Bun, Node, or TypeScript runtime dependency to backend tools.
- Keep permission admission, `ExternalDirectory` checks, cancellation,
  provider forwarding, durable Events, projection, Compat transport, and the
  synchronized TUI read model as the existing owners. Do not add a second
  tool-result store or projection.
- Keep hashline snapshots, mutation locks, duplicate guards, and no-op guards
  private, bounded, process-local, and scoped by Session/workdir plus resolved
  target path. File contents must not enter logs or error payloads.
- Preserve applicable `pi-hashline-edit` and Oh My Pi MIT attribution for the
  source-derived Rust port.

### Task/Subagent Contract

- Remove nested `inline_agent.description` from both single-member and batch
  model-facing Task schemas.
- Normalize an empty legacy nested description to absence so the exact captured
  call can spawn. Preserve the typed, side-effect-free rejection for a non-empty
  unsupported description supplied by a direct or stale caller.
- Do not change subagent authorization, model/category precedence, resident
  behavior, admission ownership, or run-tree projection.

### Read Contract

- Expose the `pi-hashline-edit` canonical schema:
  `path`, optional positive `offset`, optional positive `limit`, and optional
  `raw`; require only `path`.
- Retain hidden compatibility for pre-0.36.9 `filePath` calls. Resolve dual path
  spellings byte-for-byte: use the sole non-empty value, accept equal non-empty
  values, reject conflicting non-empty values as `ToolError::Input`, and reject
  both absent/empty. Do not trim paths.
- Preserve the old `offset = 0` behavior as a hidden compatibility spelling for
  line 1 because it is present in every captured call; do not advertise zero.
- For text, port hashline normalization, contextual XXH32 anchors, raw mode,
  offset/limit behavior, empty/oversize handling, truncation notices, invalid
  UTF-8 warnings, and snapshot updates. Preserve hya directory listing and
  image/PDF attachment behavior as explicit extensions.

### Hashline Edit Contract

- Replace fuzzy string replacement with the strict `path + edits` schema and
  behavior from `pi-hashline-edit` 0.8.3: `replace`, `append`, `prepend`, and
  `replace_text`; the edit object schemas are closed.
- Port exact anchor parsing and validation, text-hint collision checks,
  stale-anchor suggestions, bottom-up span application, duplicate/conflict
  detection, no-empty-file guard, warnings, no-op loop guard, duplicate-edit
  guard, bounded snapshot history, and exact context-3/fuzz-0 stale recovery.
- Port same-directory atomic writes, symlink and hard-link behavior, mode/BOM/
  line-ending preservation, and cleanup after failure. Keep hya formatter, LSP,
  permission, diff, and session-diff integration. Fresh anchors and stored
  snapshots must describe final post-formatter bytes.
- Support only the compatibility inputs implemented by the pinned package's
  `prepareArguments` boundary. Remove hya's fuzzy `oldString/newString` surface
  and migrate every repository caller.

### Grep Contract

- Expose the pinned schema: required `pattern`; optional `path`, `glob`,
  `ignoreCase`, `literal`, `context` in 0..=5, and `limit` in 1..=200.
- Implement the behavior natively with Rust regex and gitignore-aware traversal;
  do not require an external `rg` process. Preserve result order, regex/literal
  and case behavior, context-range merging, limit+1 truncation detection,
  hashline rows, summaries, and grep-to-edit snapshot recovery.
- Preserve hya Grep and external-directory permission checks. Add bounded
  structured display metadata for per-file TUI rendering without changing the
  Event model.

### Write And Bash Contracts

- Expose Oh My Pi's closed Write schema `path + content`. Keep hya permission,
  formatter, LSP, BOM, and whole-file semantics; use the native atomic writer,
  strip accidental hashline display prefixes, mark a shebang file executable
  when possible, and return bounded semantic metadata for presentation.
- Make `bash` the sole model-facing command tool and keep `shell` as a hidden
  runtime compatibility alias. Expose the closed base schema `command`,
  optional string map `env`, optional timeout in seconds, optional `cwd`, and
  optional `pty`.
- Port the portable Oh My Pi command contract in Rust: default 300 seconds,
  zero disables the deadline, nonzero values clamp to 1..=3600, cancellation
  terminates the process group, output capture is incrementally bounded with a
  full-output artifact when truncated, nonzero exits and timeouts remain
  structured terminal results, and `pty: true` executes through a real PTY.
- Keep hya's command and external-directory permissions. Never place environment
  values in titles, summaries, diagnostics, or the TUI.

### OpenTUI Presentation

- Add one hya-owned coding-tool presentation module. It consumes projected SDK
  `ToolPart` state only; it does not fetch, poll, replay Events, or create a
  second store.
- Render completed Read and Write fragments with a title, file-derived syntax
  highlighting, stable line numbers, and bounded expand/collapse behavior.
  Render Edit with the existing semantic diff primitive. Render Grep as
  per-file titled match/context blocks with file-derived highlighting. Render
  both `bash` and hidden `shell` through one command/output block, highlighting
  only the command and keeping output plain.
- Preserve pending, streaming, permission, denied, malformed-data, attachment,
  directory, truncation, diagnostic, error, and fallback behavior. Never render
  arbitrary input keys or `env`.
- The presentation must remain usable at 80 columns and wide terminals, and
  must render the same completed state after Session replay.

### Release And Verification

- Use red-green TDD for each changed observable contract, including the exact
  captured Read and Task inputs before production changes.
- Update backend/frontend specifications and user-facing tool/TUI documentation.
- Prepare workspace version 0.36.9. Archive the previous root changelog as
  `docs/changes/CHANGELOG_0.36.8.md`; keep root `CHANGELOG.md` limited to 0.36.9.
- Create atomic semantic commits for the subagent fix, native hashline tools,
  host Write/Bash alignment, TUI presentation, and release metadata. Push only
  after all required gates pass.
- After all verification gates pass, install the 0.36.9 binary/runtime closure
  into the user-local prefix, stop verified stale hya 0.36.8 processes without
  changing the Session database, and exercise the installed 0.36.9 surface.

### Review Hardening

- Authorize lexically external Read and Grep targets before metadata or target
  kind probing. Use one kind-blind external-directory permission resource so a
  denied request cannot reveal whether the target exists or is a file/directory.
- Bound every untrusted-work path: Hashline diagnostics, caller glob patterns,
  ignore sources/rule counts, Grep logical lines, traversal cancellation,
  nested coding-result serialization, and the final post-hook Event envelope.
- Revalidate the prepared filesystem identity immediately before destructive
  replace/truncate operations. After a committed Edit, cancellation must report
  `Cancelled` while preserving the actual bytes and synchronized snapshot.
- Bash PTY timeout/cancellation must terminate descendants even after the shell
  leader exits. Spill artifacts must be private, complete, and owned by an armed
  cleanup guard until a successful result publishes their path.
- Keep SyncProvider as the only TUI transport/store owner. Presentation must
  preserve backend truncation facts, nullable Bash termination, positioned
  error diagnostics, narrow unified diffs, plain command output, and secret
  exclusion.
- Release packaging must recursively ship the TUI source tree, include and
  verify both required notice files, and reject a prepared runtime without its
  `node_modules` dependency tree.

## Acceptance Criteria

- [ ] The exact captured four-key Read request completes with a `ToolResult`; the
      published schema contains only canonical `path`, `offset`, `limit`, and
      `raw`, requires `path`, and advertises positive offset/limit bounds.
- [ ] Canonical, legacy-only, equal dual-key, one-empty dual-key, conflict,
      missing, `offset = 0` compatibility, raw, directory, media, invalid UTF-8,
      truncation, and permission Read cases have behavior tests.
- [ ] Golden hash vectors match the pinned package, including
      `alpha/beta/gamma/delta -> KT/JB/KJ/PX`, all 2..=4 widths, Unicode trailing
      whitespace, slice-neighbor, parser, and terminal-newline cases.
- [ ] Edit tests cover every operation, exact anchors, hints/collisions,
      compound edits, conflict and error codes, no-op/duplicate guards,
      Read-to-Edit chaining, stale recovery success/conflict/no-history, atomic
      target modes, symlinks, hard links, BOM, line endings, formatter, LSP,
      diff metadata, and cancellation.
- [ ] Grep tests cover regex/literal/case/glob/gitignore, files and directories,
      no matches, context merge/separators, exact limit versus limit+1,
      cancellation, hashline output, display metadata, and Grep-to-Edit recovery.
- [ ] Write exposes only `path + content`; Bash exposes only the pinned base
      fields; `shell` is not advertised. Write atomicity/prefix stripping/
      executable metadata and Bash timeout/cancel/exit/PTY/truncation/artifact
      behavior pass focused tests.
- [ ] The exact captured Task call with an empty nested description creates a
      child and lets the parent resume. The field is absent from both published
      schemas, while a non-empty direct value retains the typed no-side-effect
      error.
- [ ] Permission ordering and typed errors remain stable, historical Events are
      not rewritten, and no new backend or presentation read model exists.
- [ ] Focused OpenTUI tests prove titles, syntax-colored spans, Read offsets,
      Edit diff mode, per-file Grep rendering, `bash`/`shell` normalization,
      ANSI-safe output, secret-field exclusion, malformed fallback, and live
      part replacement.
- [ ] External Read/Grep denial precedes metadata probing; Hashline diagnostics,
      glob/ignore input, Grep lines, traversal, coding-result envelopes, and
      post-hook results remain deterministically bounded and cancellable.
- [ ] Atomic pathname/hard-link swaps are rejected before mutation; committed
      Edit cancellation reconciles bytes/snapshots; Bash descendants and private
      spill artifacts are cleaned on timeout, cancellation, and error.
- [ ] TUI normalization/rendering preserves all backend truncation flags,
      nullable Bash output, the first three positioned severity-one diagnostics,
      80-column unified rows, command-only highlighting, and one synchronized
      message owner.
- [ ] Release validation, packaging, archive inspection, smoke checks, and the
      installer prove recursive TUI sources, byte-identical root/TUI notices,
      and a present TUI `node_modules` tree.

- [ ] A real backend plus real hya-ts PTY scenario shows all five coding tools at
      140 and 80 columns, then shows the same completed blocks after reopening
      the Session.
- [ ] Rust CI-equivalent checks, the serialized process E2E suite, TypeScript
      typecheck/tests, focused real-backend TUI tests, and Trellis validation all
      pass.
- [ ] Cargo/package metadata and newest-only changelog report 0.36.9, source
      attribution is shipped, atomic commits are pushed, and the final report
      states that the old backend must restart and old error Events remain.
- [ ] The installed user-local `hya`, `hya-backend`, `hya-ts`, and packaged TUI
      runtime report/use 0.36.9, no verified stale 0.36.8 process remains, and
      an installed-binary smoke proves the repaired Read path.

## Out Of Scope

- Rewriting, deleting, or retrying historical `ToolError` Events or modifying
  the user's live Session database.
- Oh My Pi harness-only features: internal URL/device/archive/SQLite routing,
  editor terminal bridges, direnv integration, Bash interceptors, automatic or
  explicit background jobs, and artifact URI infrastructure not already owned
  by hya.
- A user-facing hashline configuration surface. This release uses the pinned
  defaults: two-character hashes, `replace_text` enabled, warnings enabled, and
  built-in Grep always available.
- Changing the permission policy for symlink targets or adding a new security
  boundary beyond the existing lexical path policy.
- Publishing a release tag or GitHub Release.

## Technical Notes

- The source package uses process-global snapshot state. Hya scopes equivalent
  state by Session/workdir to prevent one Session from recovering content that
  it did not read; within one Session the observable hashline workflow is the
  same.
- Hya's existing global tool-output cap remains a safety boundary. Coding-tool
  results must keep anchor-bearing model output valid and retain a bounded UI
  payload when that cap applies; changing unrelated tool limits is not part of
  this task.

## Open Questions

None.
