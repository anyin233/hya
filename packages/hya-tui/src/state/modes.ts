/**
 * Session permission modes (docs/tui.md "Permission modes"): the Shift+Tab
 * cycle, the one-time yolo confirmation, the status bar text and tone of a
 * mode, the transcript notice, and the `/permissions` picker rows. Pure
 * functions; app/modes.ts runs the switch (`UpdateSession`).
 *
 * Modes: `manual`, `yolo`, and bundle modes `<bundle-id>/<mode-id>` from
 * `GET /v1/permission-modes`.
 */
import type { KeyLike } from "../keys/bindings"
import type { PickerRow } from "./picker"

/** One row of `ListPermissionModesResponse.modes`. */
export interface PermissionModeInfo {
  id: string
  title?: string
  description?: string
  /** `builtin` or the declaring bundle id. */
  source?: string
}

export const manualMode = "manual"
export const yoloMode = "yolo"

/** The built-ins, used until (or unless) the backend lists its modes. */
export const builtinModes: readonly PermissionModeInfo[] = [
  { id: manualMode, title: "Manual", description: "Ask the user before actions that need permission.", source: "builtin" },
  { id: yoloMode, title: "Yolo", description: "Allow every action without asking, including explicitly denied ones.", source: "builtin" },
]

export const yoloConfirmText = "Enable yolo? Every tool call runs without asking · Enter confirms · Esc cancels"

/** The mode in effect for the open session, else the one chosen for the next session, else manual. */
export function effectiveMode(state: { selected?: { permissionMode?: string } | undefined; pendingMode?: string | undefined }): string {
  return state.selected ? state.selected.permissionMode || manualMode : state.pendingMode || manualMode
}

/** The listing, or the built-ins when it is empty. */
function listed(listing: readonly PermissionModeInfo[]): readonly PermissionModeInfo[] {
  return listing.length ? listing : builtinModes
}

/** Shift+Tab order: manual, yolo, then the bundle modes in listing order. */
export function modeCycle(listing: readonly PermissionModeInfo[]): string[] {
  const others = listing.map((mode) => mode.id).filter((id) => id && id !== manualMode && id !== yoloMode)
  return [...new Set([manualMode, yoloMode, ...others])]
}

/** The mode after `current` (wrapping); an unknown current mode restarts at the first. */
export function nextMode(current: string, cycle: readonly string[]): string {
  const at = cycle.indexOf(current || manualMode)
  return cycle[at < 0 ? 0 : (at + 1) % cycle.length] ?? manualMode
}

/** A pending yolo confirmation: the mode asked for and the mode in effect. */
export interface ModeConfirm {
  target: string
  from: string
}

export type ModeRequest =
  | { type: "same" }
  | { type: "confirm"; confirm: ModeConfirm }
  | { type: "apply"; mode: string }

/** What switching from `current` to `target` needs: yolo asks until it was confirmed once in this process. */
export function requestMode(target: string, current: string, yoloConfirmed: boolean): ModeRequest {
  if (target === (current || manualMode)) return { type: "same" }
  if (target === yoloMode && !yoloConfirmed) return { type: "confirm", confirm: { target, from: current || manualMode } }
  return { type: "apply", mode: target }
}

export type ConfirmOutcome =
  | { type: "confirm" }
  | { type: "cancel" }
  | { type: "advance"; mode: string }
  | { type: "pass" }

export function isShiftTab(key: KeyLike): boolean {
  return (key.name === "tab" && key.shift && !key.ctrl && !key.meta) || key.sequence === "\x1b[Z"
}

/**
 * A key while the yolo confirmation shows: Enter confirms, Esc cancels,
 * Shift+Tab skips past the target to the next mode of the cycle (cancel when
 * that is the mode in effect), any other key cancels and reaches the input.
 */
export function confirmKey(key: KeyLike, confirm: ModeConfirm, cycle: readonly string[]): ConfirmOutcome {
  if (!key.ctrl && !key.meta && !key.shift && (key.name === "return" || key.name === "kpenter")) return { type: "confirm" }
  if (key.name === "escape") return { type: "cancel" }
  if (isShiftTab(key)) {
    const next = nextMode(confirm.target, cycle)
    return next === confirm.from ? { type: "cancel" } : { type: "advance", mode: next }
  }
  return { type: "pass" }
}

export type ModeTone = "normal" | "error" | "accent"

/** Status bar text and tone: manual plain, yolo `⚠ yolo` in the error color, a bundle mode its title in the accent color. */
export function modeDisplay(mode: string, listing: readonly PermissionModeInfo[]): { text: string; tone: ModeTone } {
  const id = mode || manualMode
  if (id === manualMode) return { text: manualMode, tone: "normal" }
  if (id === yoloMode) return { text: `⚠ ${yoloMode}`, tone: "error" }
  return { text: listing.find((row) => row.id === id)?.title || id, tone: "accent" }
}

/** The muted transcript line a switch adds. */
export function modeNotice(mode: string, listing: readonly PermissionModeInfo[]): string {
  const id = mode || manualMode
  if (id === manualMode || id === yoloMode) return `Permission mode → ${id}`
  const title = listing.find((row) => row.id === id)?.title
  return `Permission mode → ${title && title !== id ? `${title} (${id})` : id}`
}

/** `/permissions` picker rows: title, source, description; the mode in effect is `current`. */
export function modeRows(listing: readonly PermissionModeInfo[], current: string): PickerRow[] {
  const effective = current || manualMode
  return listed(listing).map((mode) => ({
    id: mode.id,
    label: mode.title || mode.id,
    tag: mode.source || "",
    detail: mode.description ?? "",
    current: mode.id === effective,
  }))
}
