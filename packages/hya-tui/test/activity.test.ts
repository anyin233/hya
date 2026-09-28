import { expect, test } from "bun:test"
import { TurnActivity, terminalTurnMessage } from "../src/activity"

test("an active turn animates, reports silence, and stops only for its own turn", () => {
  const activity = new TurnActivity()
  activity.start("session-1", "turn-1", 0)

  expect(activity.label(0)).toContain("Agent working · 00:00")
  expect(activity.label(250)[0]).not.toBe(activity.label(0)[0])
  expect(activity.label(15_000)).toContain("No update for 15s")

  activity.noteEvent("session-2", 16_000)
  expect(activity.label(16_000)).toContain("No update for 16s")
  activity.noteEvent("session-1", 16_000)
  expect(activity.label(16_000)).toContain("last update 0s ago")

  expect(activity.clear("session-1", "another-turn")).toBe(false)
  expect(activity.clear("session-1", "turn-1")).toBe(true)
  expect(activity.label(17_000)).toBe("")
})

test("only terminal server turn states end the activity indicator", () => {
  expect(terminalTurnMessage({ id: "turn-1", state: "TURN_STATE_RUNNING" })).toBeNull()
  expect(terminalTurnMessage({ id: "turn-1", state: "TURN_STATE_FINISHED", finish: "FINISH_REASON_STOP" })).toBe("Turn finished · stop")
  expect(terminalTurnMessage({ id: "turn-1", state: "TURN_STATE_FAILED", errorMessage: "provider rejected key" })).toBe("Turn failed · provider rejected key")
  expect(terminalTurnMessage({ id: "turn-1", state: "TURN_STATE_CANCELLED" })).toBe("Turn cancelled")
})

test("a failed backend status check is visible in the heartbeat row", () => {
  const activity = new TurnActivity()
  activity.start("session-1", "turn-1", 0)
  activity.setStatusCheck("session-1", "turn-1", false)
  expect(activity.label(5_000)).toContain("Backend status unavailable")
  activity.setStatusCheck("session-1", "turn-1", true)
  expect(activity.label(5_000)).toContain("Agent working")
})
