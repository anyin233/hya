# Spec: Rust Multi-Agent Coding Agent ("hya")

> Status: **PLANNING / brainstorm in progress.** This PRD captures the user
> request and confirmed facts. Requirements marked _(proposed)_ are not yet
> user-confirmed. Open questions drive the brainstorm.

## Goal

Build a Rust-based coding agent (CLI + rich TUI) in the style of opencode /
Claude Code, whose **two core differentiators** are:

1. **Multi-agent orchestration ("team mode")** — a main/lead model orchestrates
   multiple subagents that run **in parallel** to do different jobs, improving
   both **token efficiency** (delegated context stays in the subagent) and
   **wall-clock speed** (parallel execution). Modeled on omo's team mode.
2. **Autonomous completion** — two sibling modes driven by an independent gate:
   - **goal** — for **definitive** tasks: a stated, verifiable completion
     condition. A **single cheap verifier** judges the surfaced transcript
     **independently** and loops the worker until the condition holds (or a cap
     trips). Modeled on Claude Code's `/goal`.
   - **loop** — for **open-ended** tasks with an unclear goal but a fixed
     **iteration budget** (N iters). Each iteration is one long worker session
     followed by a **two-agent gate**: (a) a **verifier** judging how well the
     iteration's result matches the loop-target *description* (graded, + gaps),
     and (b) a **high-intelligence planner** that uses the verifier's feedback to
     plan the next iteration's directive. Loop ends at budget exhaustion (hard
     cap) or — configurably — early when the verifier reports the target is
     satisfactorily met. Richer than Claude Code's time-interval `/loop`.

User value: a fast, local, high-performance coding agent that finishes
substantial multi-step work autonomously and verifiably, while spending fewer
tokens than a single-context agent.

## Confirmed Facts (from request)

- **Language**: Rust is the primary implementation language. (hard requirement)
- **TUI**: must be rich AND high-performance, in the style of opencode's TUI.
- **Reference systems**:
  - opencode — overall coding-agent architecture & TUI feel
    (github.com/anomalyco/opencode; upstream github.com/sst/opencode)
  - oh-my-openagent (omo) — team-mode multi-agent orchestration model
    (github.com/code-yeongyu/oh-my-openagent)
  - Claude Code `/goal` — goal-driven dev / independent verification gate
    (code.claude.com/docs/en/goal)
- **Project state**: greenfield. Repo has only Trellis scaffolding + AGENTS.md;
  no Rust code, no git history yet.

### Confirmed facts about `/goal` (from docs, code.claude.com/docs/en/goal)

- `/goal <condition>` sets a session-scoped completion condition; the agent
  keeps starting new turns until met, then auto-clears.
- After **each turn**, a **small/fast model** (separate evaluator) reads the
  condition + the conversation so far and returns yes/no + a short reason.
- The evaluator **does not call tools or read files** independently — it only
  judges what the main agent surfaced in the transcript. Therefore conditions
  must be phrased so the main agent's own output can demonstrate them
  (e.g. "`cargo test` exits 0", "`git status` clean").
- Implemented as a wrapper over a session-scoped prompt-based Stop hook.
- A "no" feeds the reason back to the main agent as next-turn guidance.
- Conditions can include a bound clause ("or stop after 20 turns"); max 4000 chars.
- Goal persists across resume; turn/timer/token baselines reset on resume.

## Requirements

### Functional (all confirmed for v0)

- F1. Client/server coding agent: a `core` server (sessions, agent loop, tools,
  providers, orchestration, goal engine) + a ratatui TUI client over local
  HTTP/SSE. (D3)
- F2. **Team-mode orchestration (full parity)**: lead + 1–N writer subagents as
  isolated child sessions; ~12 `team_*` tools; in-(server-)memory mailbox + task
  board (trait-backed for future file/multi-process); category→model routing;
  skill injection; git-worktree-per-agent + tmux panes; lead/member lifecycle
  with graceful shutdown + force recovery. (D4)
- F3a. **Goal mode (definitive, full lifecycle)**: single active goal per
  session; independent cheap-model **verifier** that judges ONLY the surfaced
  transcript (no tools); turn/time cap; reason-feedback loop; set/status/clear/
  resume/non-interactive. (D4)
- F3b. **Loop mode (open-ended, budget-bounded)**: a fixed iteration budget N;
  each iteration = one worker session (which may itself spawn a team) + a
  **two-agent gate** = a cheap **verifier** (grades match to the loop-target
  description + reports gaps, transcript-only, no tools) AND a separate
  **high-intelligence planner** (consumes the verifier's feedback + iteration
  history and emits the next iteration's directive, no tools). Stops at budget
  (hard cap) or early when the verifier reports satisfied (configurable).
  set/status/clear/resume/non-interactive. (D5)
- F3-shared. Goal and loop share ONE internal iteration driver (turn driving,
  caps, cancellation, Team Evidence Envelope intake); they differ only in the
  **gate** (goal: 1 verifier → met?; loop: verifier + planner → score + next
  directive) and the **stop rule** (goal: condition-met-or-cap; loop:
  budget-or-satisfied). (D5)
- F4. **Multi-provider** abstraction (Provider/Protocol/Route + normalization to
  one internal event stream): Anthropic + OpenAI + OpenAI-compatible/local in v0;
  model-tier routing for goal-gate + categories. (D2)
- F5. Tool system (two-layer: canonical tool schema + typed runtime wrapper) with
  an allow/ask/deny permission plane and semantic approval scopes. Essential
  v0 tools: read, write, edit, shell, glob/grep, plus the orchestration tools.
- F6. Session persistence (event-sourced + projected, resume, history).
- F7. Subagent category + skill-config system feeding F2.

### Non-Functional

- N1. High-performance TUI (low latency, smooth streaming render) driven off a
  normalized message/part lifecycle + streaming event bus.
- N2. Token efficiency is a first-class design goal: model routing + context
  isolation + summary/task result flow + session-based continuation.
- N3. Rust idiomatic, type-safe, testable. No `unwrap`/`panic` in library paths;
  typed errors; parse-don't-validate at boundaries.

### Explicitly OUT of v0 (D1 breadth cut)
LSP integration, MCP servers, desktop/GUI app, web docs site, plugin system,
theming engine beyond a basic palette, multi-user/hosted/billing.

## Acceptance Criteria (for THIS spec task)

This task's deliverable is the **spec**, not running code. Done when:

- [ ] `prd.md` captures confirmed scope + testable acceptance for the product.
- [ ] `design.md` defines architecture: process model, agent/session model,
      orchestration engine, goal/verification engine, tool system, provider
      abstraction, TUI architecture — grounded in the three references.
- [ ] `implement.md` defines a phased, ordered build roadmap with milestones
      and validation gates.
- [ ] Each core feature (multi-agent orchestration, goal mode, AND loop mode with
      its two-agent verifier+planner gate) has a concrete, reviewable mechanism
      described, not hand-waving.
- [ ] Spec passes a cross-model plan review (plan-review skill) before any code.
- [ ] User reviews and approves the spec.

## Out of Scope _(proposed)_

- Implementing the agent itself (separate child tasks after spec approval).
- Cloud/hosted features, billing, multi-user.
- IDE plugins / GUI beyond the TUI.

## Resolved Decisions

- **D1 (Q1) — Scope philosophy: minimal feature BREADTH, full feature DEPTH.**
  (Reconciled after D3/D4.) We do NOT reinvent opencode's full breadth — v0 has
  NO LSP, NO MCP, NO desktop app, NO web docs, and only an essential tool set.
  BUT the architecture (client/server, multi-provider) and the two
  differentiators ship at full depth. "Lean" = narrow surface, not shallow
  features. (user-confirmed, reinterpreted)
- **D4 (Q2) — Differentiator richness: full omo + goal parity in v0.** Team mode
  ships the full ~12-tool surface, complete category system, skill injection,
  git-worktree-per-agent + tmux panes, full lead/member lifecycle (incl. graceful
  shutdown + force recovery). Goal ships the full lifecycle (set/status/clear/
  resume/non-interactive) with an independent cheap-model evaluator that judges
  ONLY the surfaced transcript. This is a large, ambitious v0. (user-confirmed)
- **D5 (loop feature) — autonomous completion has TWO modes: goal + loop.**
  goal = definitive condition + single cheap verifier (D4, as-is). loop =
  open-ended target description + iteration budget N + a two-agent per-iteration
  gate (cheap **verifier** grades match + gaps; strong **planner** plans the next
  iteration). Both share one iteration driver; they differ in gate + stop rule.
  Loop's planner runs on a STRONG model tier; the verifier on the cheap tier.
  (user-requested; see "Assumptions to confirm" for the open sub-decisions)
- **D2 (Q5) — Provider strategy: Multi-provider from day 1.** Define a clean
  `Provider` trait AND ship v0 with multiple providers (Anthropic + OpenAI +
  OpenAI-compatible/local). Model-tier routing (cheap goal-gate / per-category
  team-mode models) layers on top of this. Note: this enlarges the v0 base
  surface (per-provider auth, streaming, tool-call format normalization) — the
  design must include a provider-normalization layer (opencode's
  Provider/Protocol/Route split) so the agent loop sees one uniform
  message/tool-call shape. (user-confirmed)
- **D3 (Q3+Q6) — Runtime: Client/server split from day 1 (opencode-style).**
  A `core` server process owns sessions, the agent loop, tools, providers, the
  orchestration engine, and the goal engine; the ratatui TUI is a client over a
  local HTTP/SSE API. Enables remote control / SDK / multiple clients and
  multi-agent observability immediately, and forces a clean boundary.
  **Tension with D1**: this is a deliberately fuller skeleton, so the FEATURE
  cut (tools, # of subagent roles, category richness) must stay minimal in v0 to
  keep scope sane. Subagent orchestration runs INSIDE the server (child sessions
  as async tasks + in-memory control plane in v0), behind a trait so a
  file-backed/multi-process substrate (worktrees/tmux) can be added later.
  (user-confirmed)

## Open Questions

### Resolved (drove the scope)
- Q1 → D1 (scope philosophy). Q5 → D2 (multi-provider). Q3+Q6 → D3 (client/server
  + in-server async orchestration). Q2 → D4 (full omo+goal parity in v0).

### Deferred to design.md (design-level, not blocking scope)
- TUI stack details (ratatui assumed) + exact client↔server transport (HTTP+SSE
  vs WebSocket) — to be specified + justified in design.md.
- Goal UX surface (`/goal`-style command) + exactly how a goal turn that
  delegates to a team gets its team results summarized into the lead transcript
  for the evaluator to judge.
- Wire-protocol shape (reuse opencode-style normalized event stream).
- Naming: working name "hya" (repo dir) unless the user renames.

### Assumptions to confirm (loop feature — D5 sub-decisions)
These are my proposed defaults; correct any before/after the re-review.
- **A1 — early stop**: loop ends at budget N (hard cap) AND may stop early when
  the verifier reports the target satisfactorily met (`stop_when_satisfied`,
  default **true**). Set false to always exhaust the budget (keep refining).
- **A2 — gate independence**: both gate agents are **tool-less**; the verifier is
  transcript-only (like goal's); the planner additionally sees prior verdicts +
  an iteration-history summary (Team Evidence Envelope) but still no tools — it
  only emits the next directive.
- **A3 — iteration session model**: each iteration is one worker session seeded
  by the planner's directive; the **planner** holds cross-iteration continuity so
  worker sessions don't accumulate unbounded context (token efficiency, N2).
- **A4 — model tiers**: planner = strong tier (configurable, e.g. ultrabrain-class);
  verifier = cheap tier (shared with goal's evaluator config).
- **A5 — caps**: loop requires an explicit budget N; hard ceiling default 100
  iterations; per-iteration turn/token caps inherited from goal safety caps.
