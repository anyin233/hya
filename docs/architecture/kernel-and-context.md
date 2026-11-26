# Kernel & Context Engine

At the heart of Hya (`crates/hya-core` and `crates/hya-context`, [ADR-0033](../adr/0033-three-primitive-kernel.md)–[ADR-0035](../adr/0035-full-mode-follows-the-kernel.md)) is a deliberate split between **the irreducible agent loop** (*the Kernel*) and **everything optional layered around it** (*Full Mode*).

---

## 1. Context is $(m, g, w)$

A running session's state reduces to three pieces derived from the event log:

- **$m$ — Visible transcript:** The messages, tool calls, tool results, and compaction markers actually sent to the model on the next round.
- **$g$ — Completion criteria (`todo`):** The session's ordered checklist (`TodosUpdated`). Stored in the projection independently of $m$ so that even when old messages are wiped during a context reset, $g$ is carried forward intact.
- **$w$ — Workspace file observations:** Recorded `FilesObserved` and `FilesChanged` events tracking whether files on disk changed since the session last looked (powering conflict checks, undo snapshots, and `workspace.state` caching).

---

## 2. The Three Primitives (`hya-core::kernel`)

Every round in `hya-core` updates Context through strictly three primitives:

| Primitive | What it means | Key Events |
| --- | --- | --- |
| **1. Model append** | What the model *claims* or *plans*: streamed text, private reasoning, and requested tool calls. Never treated as ground truth about the outside world. | `StepStarted`, `Text*`, `Reasoning*`, `ToolCallRequested`, `StepFinished`, `UsageRecorded` |
| **2. Tool append** | Deterministic observations from outside the model: a tool result paired with a call, or a user prompt. Invalid JSON tool arguments, unknown tools, and cancellations become structured `ToolError`s—never synthesized fake outputs. | `ToolResult`, `ToolError`, `TodosUpdated`, `FilesChanged`, `FilesObserved`, user `Message*` |
| **3. Rewrite** | Destructive context reset when the window fills up (or when the model calls `rewrite`): drops visible history before the current step and inserts a clean system marker repeating all prior user messages verbatim plus the current `todo` list ($g$). Dropped messages stay safely in SQLite for human replay. | System marker message + `ContextCompacted { strategy: rewrite }` |

Running `hya exec --bare` runs this exact kernel loop alone—six tools (`bash`, `read`, `write`, `edit`, `todo`, `rewrite`), a minimal `<cwd>` system prompt, zero bundles, and zero sidecar processes.

---

## 3. Built-in Kernel Safeguards

Because both bare mode and full mode share `hya-core::kernel`, every agent gets three tricky edge-case protections automatically:

1. **Malformed tool-call JSON guard:** If a model cuts off mid-JSON or appends trailing garbage to tool arguments, the decoder records `{"invalid_json_arguments": ...}` and the kernel returns a clean `ToolError` telling the model to resend valid JSON—without crashing the stream or poisoning future provider requests.
2. **Dropped-reasoning detection:** Some OpenAI-compatible gateways silently strip replayed private reasoning tokens between tool rounds, causing reasoning models to suffer sudden amnesia. The kernel compares round-over-round prompt token counts (`curr.prompt < prev.prompt + prev.reasoning / 2`) and injects a one-time notice telling the model to write key findings in visible text or `todo` items instead of relying on private thinking.
3. **Reasoning-only deadline guard (`--time-budget`):** When running under a wall-clock budget, if a model spends minutes streaming pure thinking without emitting text or a tool call across a WrapUp/Final checkpoint, the kernel interrupts the step cleanly, preserves what it thought so far, and starts the next round with an explicit "act now" notice.

---

## 4. Context Reduction Ladder (80% Threshold)

Before every model round, the kernel measures window pressure against **80% of the model's `max_context`**:

```text
used = max(last round's prompt + output, usage-anchored request count, local token estimate)
threshold = max_context * 8 / 10
```

When `used > threshold`, the kernel runs registered `hya_context::rewrite::Rewriter` extensions in order, re-measures after any change, and runs its own `Rewrite` only if the prompt is *still* above 80%:

```mermaid
flowchart LR
    Check{"used > 80% window?"} -->|No| Send["Send model round"]
    Check -->|Yes, Full Mode| Shake["1. shake (hya/compaction)<br/>Spill stale tool outputs to artifact://"]
    Shake --> Re1{"Still > 80%?"}
    Re1 -->|No| Send
    Re1 -->|Yes, Responses/Codex/Grok| Remote["2. remote (hya/compaction)<br/>Provider /responses/compact"]
    Remote --> Re2{"Still > 80%?<br/>(or unsupported route)"}
    Re2 -->|No| Send
    Re2 -->|Yes (or --bare)| Rewrite["3. Kernel Rewrite<br/>Keep user prompts + todo list"]
    Rewrite --> Send
```

- **`shake` (`ContextEvicted`):** Owned by the internal native bundle `bundles/internal/compaction`. Evicts completed tool outputs outside the last `keep_recent` (default `6`) messages/steps, spilling bodies ≥ 512 bytes to `artifact://...` so the model can still `read` them back if needed. Eviction notices are persisted on `PartProjection::Tool.eviction_notice` so prompt prefixes stay byte-stable for provider KV caching!
- **`remote` (`ContextCompacted { strategy: native }`):** Calls upstream `/responses/compact` on routes that support native window folding, persisting `HYA_COMPACTED_CONTEXT` plus the active `todo` list.
- **Manual `/compact` & `/summarize`:** Acquires the session turn lease, tries `remote` first, and falls back to Kernel `Rewrite` (`threshold: 0`, marked `manual: true`).
