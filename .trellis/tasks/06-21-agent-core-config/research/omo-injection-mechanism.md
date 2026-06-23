# Research: omo (OhMyOpenCode) prompt-injection mechanism

Per user instruction ("before start, check how omo inject the prompt, we use similar
mechanism — per-agent & per-model rule"). Source: locally installed
`oh-my-openagent` / `oh-my-opencode` (omo-codex variant).
Base: `~/.config/opencode/node_modules/oh-my-openagent/packages/omo-codex/plugin/`

## Summary

omo injects context via two cooperating systems, both adapting text to the active
**agent + model + trigger**:

1. Keyword/event-triggered **directives** (e.g. `ultrawork`/`ulw`).
2. A file-discovered **rules engine** (per-source, dedup'd, budgeted).

## 1. Directive injection — `ultrawork` (the `ulw` the user referenced)

- Text: `components/ultrawork/directive.md` (the `<ultrawork-mode>` block); loaded by
  `components/ultrawork/src/directive.ts`.
- Trigger logic — `components/ultrawork/src/codex-hook.ts`:
  - Injects on `UserPromptSubmit` **only when** prompt matches `/(?:ultrawork|ulw)/i`
    (`isUltraworkPrompt`, line 82).
  - **Dedup**: skips if `<ultrawork-mode>` marker already in transcript tail
    (`hasUltraworkDirectiveAlreadyInTranscript`, line 39) → one-shot per session.
  - **Guard**: skips under context-pressure/compaction markers (line 8-16, 86-99).
  - Output: Codex hook JSON `{ hookSpecificOutput: { hookEventName, additionalContext } }`.
- **Per-MODEL variants**: compiled `dist/hooks/keyword-detector/ultrawork/{default,gpt,gemini}`
  → directive text tailored per model family.

## 2. Rules engine — per-source static/dynamic injection

- `components/rules/src/static-injection.ts`: `engine.loadStaticRules(cwd)` → filter
  already-injected / already-in-transcript (`isStaticInjected` / `markStaticInjected` /
  `filterRulesAlreadyInTranscript`) → `engine.formatStatic(rules)` → persist dedup state.
  Post-compact recovery re-injects rules missing from a compacted transcript.
- `components/rules/src/config.ts`: modes `static|dynamic|both|off`; char budgets
  (`maxRuleChars`, `maxResultChars`, + prompt/post-compact/dynamic variants); enabled
  **sources** with priority: `.omo/rules`, `.claude/rules`, `.cursor/rules`,
  `.github/instructions`, `.github/copilot-instructions.md`, `CONTEXT.md`, `plugin-bundled`,
  `~/.omo/rules`, `~/.opencode/rules`, `~/.claude/rules`.
- Rule file format = Cursor `.mdc` style. `bundled-rules/windows-git-bash.md`:
  ```
  ---
  description: Windows Git Bash guidance for Codex
  alwaysApply: true
  ---
  <body>
  ```
  (Cursor rules also support `globs:` for file-pattern-scoped application.)

## 3. Agent role schema (TOML) — `components/ultrawork/agents/*.toml`

Roles shipped: `explorer`, `librarian`, `metis`, `momus`, `plan`,
`lazycodex-{executor, code-reviewer, clone-fidelity-reviewer, gate-reviewer, qa-executor}`.

Schema (from `plan.toml` / `librarian.toml`):
```toml
name = "plan"
description = "..."
nickname_candidates = ["Planner"]
model = "gpt-5.5"
model_reasoning_effort = "xhigh"   # plan.toml
service_tier = "fast"              # librarian.toml
developer_instructions = """ <full system prompt body> """
```
- **Tool policy is PROSE** inside `developer_instructions` ("Tools I will NEVER call:
  edit/write/apply_patch…"), NOT a structured allowlist. Skills referenced in prose too.

## Distilled model for yaca (in-process engine — no hook IPC needed)

- `AgentCore { name, model, prompt, allowed_tools, skills, injections }`.
- **Injection directive** = library file with frontmatter selectors
  `{ agents, models, trigger(always|session-start|keyword), keyword(regex), once, priority }`
  + body (+ optional per-model variants `name.<family>.md`).
- At prompt-build (and per user prompt for `keyword` triggers): select every directive whose
  `agents` ∧ `models` match the active `(name, model)` and whose `trigger` fires; inject bodies
  by `priority`; dedup by `once`. `ulw` = `{ trigger: keyword, keyword: (?i)(ultrawork|ulw),
  once: true, agents: [lead] }`.
- **yaca improves on omo**: structured + **enforced** `allowed_tools` (advertise-filter +
  execution reject), vs omo's prose tool policy.
- **Defer** (vs omo): char budgets/truncation, dynamic rules, post-compact recovery
  (yaca compaction is its own pi-parity wave).
