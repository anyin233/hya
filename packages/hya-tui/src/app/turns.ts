/**
 * Prompt admission, the client-side prompt queue, and turn-state tracking.
 *
 * The server has no prompt queue: `CreateTurn` answers `409 session_busy`
 * while a turn runs. Every prompt therefore goes through a local queue that
 * is drained one prompt at a time:
 *
 * - A prompt submitted while a turn runs (or while others wait) stays
 *   `queued`; the transcript shows it dimmed with a `queued` label.
 * - A turn ends at the first assistant `messageFinished` after the turn's
 *   user message whose finish is not `FINISH_REASON_TOOL_CALLS` (a tool-call
 *   round continues the turn). `CreateTurn` returns the *user* message id as
 *   the turn id, so the returned id is never itself a turn end.
 * - After the end the next prompt is sent. The server's run guard releases
 *   slightly after `messageFinished`, so `409 session_busy` is retried with a
 *   short backoff. A prompt that still meets a busy session afterwards stays
 *   queued until the next observed turn end.
 */
import { HttpError, type HyaClient } from "../client"
import { assistantRole, toolCallsFinish, type FinishedInfo, type OverlayEffect } from "../state/overlay"
import type { AppStore, QueuedPrompt } from "../state/store"

export interface TurnRunnerOptions {
  store: AppStore
  client: Pick<HyaClient, "createTurn">
  sleep?: (ms: number) => Promise<void>
  /** Delays between `409 session_busy` retries; one retry per entry. */
  backoffMs?: number[]
}

export const defaultBackoffMs = [100, 150, 250, 400, 600, 800, 1000, 1000, 1000, 1000]

function isSessionBusy(error: unknown): boolean {
  return error instanceof HttpError && error.status === 409 && /session_busy/.test(error.message)
}

/** Status text for a finished turn. */
export function turnEndStatus(end: FinishedInfo, error: { code: string; message: string } | undefined): string {
  if (error) return `Error · ${error.code ? `${error.code}: ` : ""}${error.message}`
  switch (end.finish) {
    case "FINISH_REASON_ERROR": return "Error · turn failed"
    case "FINISH_REASON_CANCELLED": return "Cancelled · Ready"
    case "FINISH_REASON_LENGTH": return "Ready · reply stopped at the length limit"
    default: return "Ready"
  }
}

export function createTurnRunner({ store, client, sleep = (ms) => Bun.sleep(ms), backoffMs = defaultBackoffMs }: TurnRunnerOptions) {
  let draining: Promise<void> = Promise.resolve()
  let active = false
  /** Wakes a pending busy-retry sleep early when some turn ends. */
  let wake: (() => void) | undefined

  const status = (text: string): void => store.setStatus(text)
  const waiting = (): QueuedPrompt[] => store.state.queued.filter((item) => item.state === "queued")

  function runningStatus(): string {
    const count = waiting().length
    return `Running · ${store.state.turnId}${count ? ` · ${count} queued` : ""}`
  }

  /** The turn ended: show its outcome and send the next queued prompt. */
  function complete(end: FinishedInfo): void {
    const error = store.fold.error(end.message)
      ?? store.state.messages.find((message) => message.id === end.message)?.error
    store.endTurn()
    status(turnEndStatus(end, error))
    drain()
  }

  function pause(ms: number): Promise<void> {
    return new Promise<void>((resolve) => {
      wake = () => resolve()
      void sleep(ms).then(() => resolve())
    }).finally(() => { wake = undefined })
  }

  /** Admit one queued prompt; returns false when the session stayed busy. */
  async function send(item: QueuedPrompt): Promise<boolean> {
    store.beginTurn()
    store.setQueuedState(item.id, "sending")
    status("Sending prompt…")
    for (let attempt = 0; ; attempt++) {
      if (store.state.selected?.id !== item.session) return true
      try {
        const turn = await client.createTurn(item.session, item.text)
        if (store.state.selected?.id !== item.session) return true
        store.dequeue(item.id)
        // The whole turn may already have streamed past before the response.
        const ended = store.fold.turnEnd(turn.id)
        store.setTurn(turn.id)
        if (ended) complete(ended)
        else status(runningStatus())
        return true
      } catch (error) {
        if (store.state.selected?.id !== item.session) return true
        if (!isSessionBusy(error)) {
          store.dequeue(item.id)
          store.endTurn()
          status(`Error: ${String(error)}`)
          return true
        }
        store.setQueuedState(item.id, "queued")
        const delay = backoffMs[attempt]
        if (delay === undefined) {
          store.endTurn()
          const count = waiting().length
          status(`Session busy · ${count} queued prompt${count === 1 ? "" : "s"} wait${count === 1 ? "s" : ""} for the running turn`)
          return false
        }
        status(`Session busy · retrying (${attempt + 1})`)
        await pause(delay)
        store.setQueuedState(item.id, "sending")
      }
    }
  }

  /** Send queued prompts, one per turn, while the session is free. */
  function drain(): void {
    if (active) return
    active = true
    draining = (async () => {
      try {
        while (!store.state.running) {
          const next = waiting()[0]
          if (!next) return
          if (!(await send(next))) return
        }
      } finally {
        active = false
      }
    })()
  }

  return {
    /** Queue a prompt for the selected session and send it when the session is free. */
    async submit(text: string): Promise<void> {
      const session = store.state.selected?.id
      if (!session) throw new Error("No session is open")
      store.enqueue(text, session)
      if (store.state.running || active) {
        status(store.state.turnId ? runningStatus() : `Queued · ${waiting().length} waiting`)
        return
      }
      drain()
      await draining
    },

    /** React to a folded stream event: detect the end of the running turn. */
    observe(effect: OverlayEffect): void {
      const finished = effect.finished
      if (!finished || finished.finish === toolCallsFinish) return
      const role = finished.role ?? store.state.messages.find((message) => message.id === finished.message)?.role
      if (role !== undefined && role !== assistantRole) return
      const turnId = store.state.turnId
      if (turnId) {
        // Ours when it follows our user message; if that message was never
        // seen on the stream, any final assistant finish ends the turn.
        const end = store.fold.knows(turnId) ? store.fold.turnEnd(turnId) : finished
        if (end) complete(end)
        return
      }
      if (store.state.running) {
        // Admission in flight: `send` checks the fold once CreateTurn
        // returns. Another client's turn ended — retry a busy prompt now.
        wake?.()
        return
      }
      if (waiting().length) drain()
    },

    /** Resolves when the current drain (if any) settles. For tests. */
    idle(): Promise<void> { return draining },
  }
}

export type TurnRunner = ReturnType<typeof createTurnRunner>
