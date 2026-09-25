/**
 * Picker row builders for `/model`, `/agent`, and `/sessions` (S9, C11–C13):
 * models tagged by provider, visible (non-hidden) agents with their default
 * model, and sessions as a nested tree (S6's nesting rules, state/format.ts
 * `sessionTree`) with a relative update time and a busy marker. Pure; the
 * commands (`commands/native.ts`) build the current-value id, and
 * `state/picker.ts`/`components/Picker.tsx` render the rows.
 */
import type { AgentSummary, ModelSummary, SessionInfo } from "../client"
import { modelReference, sessionTree } from "./format"
import type { PickerRow } from "./picker"

/** `/model` picker rows: one per model, tagged with its provider id; `current` is the session's `provider/model`. */
export function modelRows(models: readonly ModelSummary[], current: string): PickerRow[] {
  return models.map((model) => ({
    id: model.id,
    label: model.displayName || model.modelId || model.id,
    tag: model.providerId || model.id.split("/")[0] || "",
    detail: model.contextLimit && model.contextLimit !== "0" ? `${Math.round(Number(model.contextLimit) / 1000)}k ctx` : "",
    current: model.id === current,
  }))
}

/** `/agent` picker rows: visible agents only, tagged with the default `provider/model`; `current` is the session's agent name. */
export function agentRows(agents: readonly AgentSummary[], current: string): PickerRow[] {
  return agents
    .filter((agent) => !agent.hidden)
    .map((agent) => ({
      id: agent.name,
      label: agent.name,
      tag: agent.model?.providerId && agent.model.modelId ? `${agent.model.providerId}/${agent.model.modelId}` : "",
      detail: agent.description ?? "",
      current: agent.name === current,
    }))
}

const minute = 60
const hour = 60 * minute
const day = 24 * hour

/** `now - time` as `Ns`/`Nm`/`Nh`/`Nd` (floor, non-negative); `""` when `time` is empty or unparsable. */
export function relativeTime(time: string | undefined, now: number = Date.now()): string {
  if (!time) return ""
  const then = Date.parse(time)
  if (Number.isNaN(then)) return ""
  const seconds = Math.max(0, Math.floor((now - then) / 1000))
  if (seconds < minute) return `${seconds}s`
  if (seconds < hour) return `${Math.floor(seconds / minute)}m`
  if (seconds < day) return `${Math.floor(seconds / hour)}h`
  return `${Math.floor(seconds / day)}d`
}

/** `/sessions` picker rows: a `New session` row first, then the tree (subagents indented and tagged `subagent`, busy noted in the detail). */
export function sessionRows(sessions: readonly SessionInfo[], current: string | undefined, now: number = Date.now()): PickerRow[] {
  const newRow: PickerRow = { id: "__new__", label: "New session", tag: "new", detail: "Create a session with the current agent and model" }
  const rows = sessionTree(sessions).map(({ session, depth }): PickerRow => {
    const detail = [
      session.agent,
      modelReference(session) || "default",
      relativeTime(session.timeUpdated, now) || undefined,
      session.busy ? "● running" : undefined,
    ].filter(Boolean).join(" · ")
    return {
      id: session.id,
      label: depth ? `${"  ".repeat(depth - 1)}↳ ${session.title || session.id}` : (session.title || session.id),
      tag: depth ? "subagent" : "",
      detail,
      current: session.id === current,
    }
  })
  return [newRow, ...rows]
}
