# Documentation synchronization design

## Design objective

Treat documentation as a projection of the checked-out repository, not as an
independent source of behavior. The implementation will update current prose
from stable source, test, help, metadata, and release evidence while retaining
historical measurements and rationale. The active task remains documentation
only; no product behavior is changed.

## Source-of-truth hierarchy

Use this precedence when two documents disagree:

1. Executable source and serialized contracts: Rust/TypeScript implementation,
   public types, route registration, and package metadata.
2. Contract tests and fixtures: focused unit/integration tests, `docs_example`,
   `matrix.toml`, installer tests, and release rehearsal tests.
3. CLI help and operational configuration: clap help output, `Cargo.toml`,
   `package.json`, `install.sh`, CI/release workflows, and xtask commands.
4. Current canonical documentation: `docs/architecture/server-client.md` for
   route detail, `docs/testing/agent-matrix.md` for scenario/tool count, and
   `docs/workflows.md` for already contract-tested Workflow prose.
5. Indexes, wiki mirrors, package READMEs, ADRs, and specs: synchronize them to
   the sources above, but do not create a second contract when a canonical page
   can be linked.
6. Historical documents are evidence about a former state, never a source for
   present tense claims. Preserve `docs/changes/`, archived task files, dated
   coverage tables, and historical plans.

Each changed claim should have a source anchor in the findings ledger. When a
source is a generated or machine-specific artifact, label its date/scope rather
than presenting it as an evergreen guarantee.

## Current architecture and document graph

The synchronized architecture graph is:

```text
hya exec shim
  -> hya-ts launcher/supervisor
    -> hya-backend runtime + HTTP/SSE server
      -> packages/hya-tui-ts (TypeScript/OpenTUI presentation)

hya-workflow (source compilation + normalized plans)
  -> hya-core (durable Workflow execution and replay)
  -> hya-app (WorkflowControl, admission, command/tool integration)
  -> hya-server (native and Compat surfaces)

hya-bundle (AgentBundle/WorkflowBundle package model)
  -> package-bundle/release tooling and prepared runtime payload

hya-client / hya-sdk / hya-native
  -> separate native, Compat, JSONL bridge, and in-process transport boundaries
```

`docs/architecture/server-client.md` remains the one detailed route table.
Other architecture pages describe ownership and link there. Native Workflow
routes and Compat route wrappers must remain separate. The Compat parity page
must not turn native-only Workflow features into a Compat claim.

The runtime event graph must state that Workflow route outcomes are bounded
replay data and that `ContextCompacted` is a durable checkpoint/baseline marker.
The TUI graph must state that synchronized state flows through the existing SDK
and contexts; the TypeScript package does not discover or spawn the backend.

## Claim normalization

For each edit, use this compact structure where useful:

```text
Claim: what the current document says.
Evidence: exact source/test/help/config/release path and line/symbol.
Scope: release/version/date, native vs Compat, or package boundary.
Action: precise wording/link/table change, or explicit defer decision.
```

Use stable symbols/headings in new testing evidence. Avoid copying volatile line
numbers into user-facing prose when a symbol, test name, or command is available.
Keep the current `0.36.7` version visible in release/source context and label the
latest published `v0.35.1` separately.

## No-drift review rule

Every current document in the audit inventory must be reviewed against the
ledger. A document with no confirmed drift is an intentional no-op: leave its
content unchanged and record it in the reconciliation report. Do not rewrite a
matching page merely to make it appear active. Only update an index when a child
path or link actually changes.


## Ordered document batches

Each batch has one writer for its document tree. Batches are ordered because
indexes and cross-links depend on canonical wording established earlier.

### B0. Evidence and scope lock

Read `prd.md`, this design, `implement.md`, `findings.md`, and the independent
inventory. Freeze the list of objective corrections and deferred decisions.
Record any newly discovered claim in findings before editing another document.
Do not edit source, tests, package behavior, release workflows, or excluded
history.

### B1. Root, glossary, and indexes

Update only the affected sections of:

- `README.md`, `DESIGN.md`, `CONTEXT.md`, and current `CHANGELOG.md`;
- `docs/README.md`, `docs/development.md`, and `docs/project-structure.md`;
- affected current indexes under `docs/` and `.autors/hya/wiki/`.

Align the runtime chain, version/publication status, package payload, prompt and
border behavior, category registry, Workflow route terminology, component map,
client/transport boundaries, 28-tool list, and `xtask` command invocation. Keep
root changelog newest-only and retain old release notes in `docs/changes/`.

### B2. Runtime, API, provider, Workflow, tool, and Compat pages

Update confirmed drift in:

- `docs/architecture/{overview,event-model,runtime,providers,agent-tool-surface,tools-and-permissions}.md`;
- `docs/cli.md`, `docs/configuration.md`, `docs/workflows.md` only where the
  ledger identifies an objective mismatch;
- `docs/compat-parity.md` and `docs/plugin-protocol.md`.

Add the missing event/context/route-outcome/tool/Skill facts, CLI synopsis and
`/workflow` catalog, adapter resolution order, and 28-name inventory. Explain
same-route HTTP retry versus engine cross-model fallback without exposing
unapproved internal numeric limits. Preserve the suffix-free model-id behavior
and mark “fully-qualified” as convention unless a product task changes the
compiler. Link to the canonical server-client route table instead of making
parallel route inventories.

### B3. TUI, packaging, and user workflows

Update confirmed drift in:

- `docs/getting-started.md`, `docs/tui-reference.md`, `docs/tui-keybindings.md`,
  `docs/agent-bundle-authoring.md`, `docs/examples/self-update/README.md`, and
  `packages/hya-tui-ts/UPSTREAM.md`;
- `docs/adr/0005-drop-legacy-tui-surface.md` and
  `docs/adr/0006-tui-session-reset-and-subagent-visibility.md` only for current
  behavior statements, preserving historical rationale;
- `packages/hya-tui-ts/README.md` and `packages/hya-tui-ts/test/README.md`;
- the affected sections of `docs/architecture/tui.md`.

The `UPSTREAM.md` prose must state the observed NOTICE difference: `install.sh`
includes and verifies `NOTICE`, while the `0.36.7` release workflow and release
rehearsal do not. This is documentation of current behavior, not a packaging
change.


Document TypeScript-only interactive ownership, 12 declared/11 default plugins
with Workflow sidebar order, aliases and dispatch precedence, 14 test files,
prepared-runtime packaging, Compat adapter payload, deterministic
`package-bundle`, correct checkout invocation, and the updater omission from
standard release archives. The self-update README must show the required
stage/report/discard/activate sequence without claiming that a source example
ships in the release archive.


### B4. Testing, release, and Trellis guidance

Before editing `.trellis/workflow.md`, load and follow `trellis-meta`. Before
editing any `.trellis/spec/**` file, load `trellis-update-spec` and the
applicable spec workflow (including `trellis-before-dev` when package/layer
context is needed). These skill gates protect generated/managed boundaries and
keep code-spec updates executable and source-backed.

Update confirmed drift in:

- `docs/testing/{README,agent-matrix,coverage-baseline,process-e2e}.md`;
- `.trellis/spec/backend/{index,quality-guidelines,workflow-control,task-tool}.md`;
- `.trellis/spec/frontend/{directory-structure,component-guidelines,quality-guidelines}.md`;
- `.trellis/workflow.md` and `.trellis/spec/guides/code-reuse-thinking-guide.md`.

Add T2.14/P19, T0-T3 range, dated coverage language, stable flake anchors, CI
resource-cap wording, release adapter/Argus assertions, exact rehearsal command,
active spec statuses, and correct archived/template paths. Remove obsolete
retained-Rust-TUI claims from active specs. Mark upstream template guidance as
such. Do not edit archived targets.


### B5. Wiki, package README, and tracked-agent boundary

Audit all `.autors/hya/wiki/pages/**/*.md` and package/crate READMEs. Edit only
objective current claims and their indexes. Keep the machine-specific
rust-toolchain page as Agent-authored evidence until its owner decides the
policy.

#### B5a. Current wiki and package README review

Review every current wiki/package README document and leave no-drift documents
unchanged. Update only confirmed contradictions and directly affected indexes.

#### B5b. `AGENTS.md` current-document update

`AGENTS.md` is in scope. Before editing it, load both `agent-dotfile` and
`writing-for-agents`; use the dotfile workflow to distinguish the repository-root
instructions from installed `agents/*` dotfiles, preserve the managed Trellis
block byte-for-byte, and edit only the current component/verification guidance
identified in findings. Run the workflow's install/sync verification (including
dry-run install, scenario, shell syntax, and secret checks). Do not hand-edit
the file outside that workflow and do not modify unrelated agent mirrors.

## Compatibility and migration rules

- Preserve native/Compat field names and route contracts. Documentation may
  cross-link the surfaces but must not imply that native Workflow extensions are
  Compat parity.
- Preserve `src/hya` versus `src/upstream` ownership and state synchronization
  through `@opencode-ai/sdk/v2`.
- Preserve root `CHANGELOG.md` newest-only policy and all immutable historical
  paths. Correct current references to archived task locations rather than
  editing the archive.
- Treat package license files and private component versions as observed facts;
  do not invent a legal or release synchronization policy.
- `packages/hya-tui-ts/UPSTREAM.md` explicitly records that `install.sh` includes
  and verifies `NOTICE`, while the `0.36.7` release workflow/rehearsal does not.
  Keep packaging behavior unchanged; the discrepancy remains an adjacent
  packaging follow-up rather than an undocumented fact.
- Keep the `cargo xtask` usage-string mismatch as an adjacent source decision;
  current docs must use the working `cargo run -p xtask -- ...` form and must
  not claim behavior that the checked-out release workflow does not perform.
- Do not add a queued-prompt handler, matrix gate, provider retry behavior,
  model-id validation, or release payload file as part of documentation work.

## Risks and rollback shape

| Risk | Prevention | Rollback |
| --- | --- | --- |
| A current page repeats stale wording from another page | Batches have a canonical source page and final cross-document scan | Restore only the affected document batch from the pre-edit diff, then reapply corrected wording |
| Historical evidence is accidentally rewritten | Keep immutable paths in a denylist and review `git diff -- docs/changes .trellis/tasks/archive docs/superpowers` | Revert only the accidental historical-file hunk; never reset unrelated user work |
| Native and Compat behavior is conflated | Keep separate sections and route-table ownership in B2 | Restore the affected parity/route section and re-link to canonical docs |
| Generated or machine-specific measurements become evergreen claims | Include version/date/scope in every measurement | Replace with the dated baseline wording from findings |
| Package/release behavior is misrepresented | Compare installer and release workflow before B3; record NOTICE/updater gaps as decisions | Remove unsupported claim; leave behavior unchanged |
| AGENTS managed content is overwritten | Run the dotfile skill workflow only, with managed markers and sync checks | Use that workflow's restore/sync procedure; do not hand-edit outside it |
| Broad link replacement introduces a wrong path | Apply exact links from findings and run link/anchor scan | Revert only the path batch and repair from the archived/current path table |

The implementation should create reviewable, narrow diffs per batch. A failed
verification gate stops subsequent wording changes until the ledger and affected
batch are corrected; it does not justify changing product code or weakening a
check.
