/**
 * Switching the session tree's permission mode (docs/tui.md "Permission
 * modes"): `/permissions` picks, `/permissions <mode>`
 * sets. The switch is `UpdateSession` with `{permissionMode}`; the reply's
 * `permissionMode` becomes the shown mode, a muted notice is added to the
 * transcript, and the pending interactions are re-listed at once (switching
 * to `yolo` makes the backend allow the tree's waiting asks, so their prompts
 * close without waiting for the `interactionResolved` frames).
 *
 * Switching to `yolo` shows a one-line confirmation the first time in this
 * TUI process (state/modes.ts `requestMode`). With no session open, the
 * choice is remembered (`pendingMode`) and applied right after the next
 * session is created (`applyPending`, called by the controller's
 * `newSession`).
 */
import type { HyaClient } from "../client"
import type { KeyLike } from "../keys/bindings"
import { confirmKey, effectiveMode, manualMode, modeCycle, modeDisplay, nextMode, requestMode } from "../state/modes"
import type { AppStore } from "../state/store"

export interface ModeSwitcherOptions {
  store: AppStore
  client: Pick<HyaClient, "updateSession" | "listInteractions">
  /** Saved default for newly created sessions. */
  preferredMode?: string
  /** Persist a successfully selected mode as the default for future TUI processes. */
  saveMode?: (mode: string) => void
}

export function createModeSwitcher({ store, client, preferredMode, saveMode }: ModeSwitcherOptions) {
  /** The user confirmed yolo once in this process: later switches to it do not ask again. */
  let yoloConfirmed = false
  let pending: Promise<void> = Promise.resolve()
  let rememberedMode = preferredMode
  if (rememberedMode && !store.state.selected) store.setPendingMode(rememberedMode)

  function remember(mode: string): string | undefined {
    if (rememberedMode === mode) return undefined
    rememberedMode = mode
    try {
      saveMode?.(mode)
    } catch (error) {
      return String(error)
    }
    return undefined
  }

  /** The mode in effect for the open session, else the one chosen for the next session, else manual. */
  function current(): string {
    return effectiveMode(store.state)
  }

  function label(mode: string): string {
    return modeDisplay(mode, store.state.permissionModes).text
  }

  async function apply(mode: string): Promise<void> {
    const selected = store.state.selected
    if (!selected) {
      store.setPendingMode(mode)
      store.setStatus(`Permission mode → ${mode} · applies when the session is created`)
      return
    }
    try {
      const info = await client.updateSession(selected.id, { permissionMode: mode })
      if (store.state.selected?.id !== selected.id) return
      const applied = info.permissionMode || mode
      store.applyPermissionMode(applied)
      const saveError = remember(applied)
      store.setStatus(saveError
        ? `Permission mode → ${label(applied)} · default could not be saved: ${saveError}`
        : `Permission mode → ${label(applied)} · /permissions lists`)
      // Asks the switch resolved (yolo allows them once) close now, not at the next frame.
      store.setInteractions(await client.listInteractions())
    } catch (error) {
      store.setStatus(`Permission mode failed: ${String(error)}`)
    }
  }

  /** Switch to `mode`; yolo asks first unless it was confirmed before (or `confirmed`). */
  function request(mode: string, options: { confirmed?: boolean } = {}): Promise<void> {
    if (options.confirmed) yoloConfirmed = true
    const decision = requestMode(mode, current(), yoloConfirmed)
    store.setModeConfirm(undefined)
    if (decision.type === "same") {
      const saveError = store.state.selected ? remember(mode) : undefined
      store.setStatus(saveError
        ? `Permission mode is already ${label(mode)} · default could not be saved: ${saveError}`
        : `Permission mode is already ${label(mode)}`)
      return Promise.resolve()
    }
    if (decision.type === "confirm") {
      store.setModeConfirm(decision.confirm)
      return Promise.resolve()
    }
    pending = apply(decision.mode)
    return pending
  }

  /** Shift+Tab: the next mode of manual → yolo → bundle modes → manual. */
  function cycle(): void {
    void request(nextMode(current(), modeCycle(store.state.permissionModes)))
  }

  /**
   * A key while the yolo confirmation shows. Returns true when the key was
   * used (Enter, Esc, Shift+Tab); any other key cancels the confirmation and
   * returns false so it reaches the input.
   */
  function key(event: KeyLike): boolean {
    const confirm = store.state.modeConfirm
    if (!confirm) return false
    const outcome = confirmKey(event, confirm, modeCycle(store.state.permissionModes))
    store.setModeConfirm(undefined)
    switch (outcome.type) {
      case "confirm":
        yoloConfirmed = true
        pending = apply(confirm.target)
        return true
      case "advance":
        void request(outcome.mode)
        return true
      case "cancel":
        store.setStatus(`Permission mode unchanged · ${label(confirm.from)}`)
        return true
      case "pass":
        store.setStatus(`Permission mode unchanged · ${label(confirm.from)}`)
        return false
    }
  }

  /** Apply the pending choice or saved default after `CreateSession`. */
  async function applyPending(): Promise<void> {
    const mode = store.state.pendingMode ?? rememberedMode
    if (!mode || !store.state.selected) return
    store.setPendingMode(undefined)
    if (mode === (store.state.selected.permissionMode || manualMode)) {
      const saveError = remember(mode)
      if (saveError) store.setStatus(`Permission mode ${label(mode)} · default could not be saved: ${saveError}`)
      return
    }
    await apply(mode)
  }

  return {
    current,
    cycle,
    request,
    key,
    applyPending,
    /** Resolves when the last switch request settled. For tests. */
    idle: (): Promise<void> => pending,
  }
}

export type ModeSwitcher = ReturnType<typeof createModeSwitcher>
