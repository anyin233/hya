/**
 * The working indicator (docs/tui.md "Working indicator"): while a turn this
 * client admitted runs, one line above the prompt dock shows the spinner,
 * elapsed time, and what the model is doing right now.
 *
 * Priority order (highest first):
 * 1. A prompt is pending for the open session's tree (`currentPrompt`): the
 *    model is blocked on the user.
 * 2. The streaming assistant message's last block is a running `task` card
 *    whose child is starting or running: the model is blocked on a subagent.
 * 3. The last block is a running (or still-streaming) tool call: `Running
 *    <tool> <summary>`.
 * 4. The last block is reasoning still streaming: `Thinking…`.
 * 5. The last block is text: `Writing…`.
 * 6. Anything else while the turn runs (no blocks yet, or between parts):
 *    `Thinking…`.
 *
 * Pure TypeScript (no Solid); `now` is passed in so elapsed time is testable.
 */
import type { Block } from "./messages"
import { transcriptViews } from "./messages"
import { childStatus, taskLink } from "./members"
import { currentPrompt } from "./prompts"
import type { AppState } from "./store"

function pad2(value: number): string {
  return String(value).padStart(2, "0")
}

/** Elapsed time as `m:ss` (or `h:mm:ss` past an hour). */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  return hours > 0 ? `${hours}:${pad2(minutes)}:${pad2(seconds)}` : `${minutes}:${pad2(seconds)}`
}

/** The subagent wait text for a `task` block whose child is starting or running; `undefined` otherwise. */
function subagentActivity(block: Extract<Block, { kind: "tool" }>, state: AppState): string | undefined {
  const task = block.card.task
  if (!task) return undefined
  const link = taskLink({ ...(block.callId ? { callId: block.callId } : {}), ...(task.child ? { child: task.child } : {}) }, state.members)
  const child = link.child ? state.children.get(link.child) : undefined
  const status = childStatus(link.member, child)
  return status === "running" || status === "starting" ? `Waiting for subagent ${task.agent}` : undefined
}

/** What the model (or the user) is doing right now, or `undefined` when nothing is running. */
export function activityText(state: AppState): string | undefined {
  const prompt = currentPrompt(state)
  if (prompt) return prompt.view.kind === "question" ? "Waiting for an answer" : "Waiting for approval"
  if (!state.selected) return undefined
  const active = [...transcriptViews(state)].reverse().find((view) => view.role === "assistant" && view.streaming)
  const last = active?.blocks.at(-1)
  if (last?.kind === "tool") {
    const waiting = subagentActivity(last, state)
    if (waiting) return waiting
    if (last.card.status === "running" || last.card.status === "pending") {
      return `Running ${last.card.tool}${last.card.summary ? ` ${last.card.summary}` : ""}`
    }
    return "Thinking…"
  }
  if (last?.kind === "reasoning") return last.active ? "Thinking…" : "Writing…"
  if (last?.kind === "text") return "Writing…"
  return "Thinking…"
}

/** Prompts still queued (not yet sending). */
function queuedCount(state: AppState): number {
  return state.queued.filter((item) => item.state === "queued").length
}

/**
 * The full working-line text (without the spinner glyph, which the component
 * draws itself): `<elapsed> · <activity>[ · Queued N] · Esc to interrupt`.
 * `undefined` when no turn admitted by this client is running.
 */
export function workingLineText(state: AppState, now: number): string | undefined {
  if (!state.running) return undefined
  const activity = activityText(state)
  if (!activity) return undefined
  const elapsed = formatElapsed(now - (state.turnStartedAt ?? now))
  const queued = queuedCount(state)
  return `${elapsed} · ${activity}${queued ? ` · Queued ${queued}` : ""} · Esc to interrupt`
}
