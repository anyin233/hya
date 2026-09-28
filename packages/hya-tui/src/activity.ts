/** Local turn heartbeat; silence means no event was received, not that the model stopped. */
import type { TurnInfo } from "./client"

const frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
const quietThresholdMs = 15_000

interface ActiveTurn {
  session: string
  turn: string
  startedAt: number
  lastEventAt: number
  statusAvailable: boolean
}

export class TurnActivity {
  private active?: ActiveTurn

  start(session: string, turn: string, now: number): void {
    this.active = { session, turn, startedAt: now, lastEventAt: now, statusAvailable: true }
  }

  matches(session: string, turn: string): boolean {
    return this.active?.session === session && this.active.turn === turn
  }

  noteEvent(session: string, now: number): void {
    if (this.active?.session === session) this.active.lastEventAt = now
  }

  setStatusCheck(session: string, turn: string, available: boolean): void {
    if (this.matches(session, turn) && this.active) this.active.statusAvailable = available
  }

  clear(session: string, turn: string): boolean {
    if (!this.matches(session, turn)) return false
    this.active = undefined
    return true
  }

  reset(): void { this.active = undefined }

  label(now: number): string {
    const active = this.active
    if (!active) return ""
    const elapsed = Math.max(0, now - active.startedAt)
    const quiet = Math.max(0, now - active.lastEventAt)
    const elapsedSeconds = Math.floor(elapsed / 1_000)
    const quietSeconds = Math.floor(quiet / 1_000)
    const clock = `${String(Math.floor(elapsedSeconds / 60)).padStart(2, "0")}:${String(elapsedSeconds % 60).padStart(2, "0")}`
    const frame = frames[Math.floor(elapsed / 250) % frames.length]
    if (!active.statusAvailable) return `${frame} Backend status unavailable · ${clock} elapsed · last update ${quietSeconds}s ago`
    return quiet >= quietThresholdMs
      ? `${frame} No update for ${quietSeconds}s · ${clock} elapsed · /cancel to stop`
      : `${frame} Agent working · ${clock} elapsed · last update ${quietSeconds}s ago`
  }
}

/** The server owns terminal turn state; message frames alone do not end a turn. */
export function terminalTurnMessage(turn: TurnInfo): string | null {
  switch (turn.state) {
    case "TURN_STATE_FINISHED":
      return `Turn finished · ${turn.finish?.replace(/^FINISH_REASON_/, "").toLowerCase() ?? "done"}`
    case "TURN_STATE_FAILED":
      return `Turn failed · ${turn.errorMessage || turn.errorCode || "unknown error"}`
    case "TURN_STATE_CANCELLED":
      return "Turn cancelled"
    default:
      return null
  }
}
