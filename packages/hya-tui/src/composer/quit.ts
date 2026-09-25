/**
 * The Ctrl+C double-press state machine. The first press clears the input
 * (`clear`) or, on an empty input, only shows the quit hint (`hint`); either
 * arms the guard. A second press within `windowMs` quits.
 */
export type CtrlCOutcome = "clear" | "hint" | "quit"

export const quitWindowMs = 2_000

export interface QuitGuardOptions {
  windowMs?: number
  now?: () => number
}

export function createQuitGuard({ windowMs = quitWindowMs, now = Date.now }: QuitGuardOptions = {}) {
  let armedAt: number | undefined
  const armed = (): boolean => armedAt !== undefined && now() - armedAt <= windowMs
  return {
    press(inputEmpty: boolean): CtrlCOutcome {
      if (armed()) {
        armedAt = undefined
        return "quit"
      }
      armedAt = now()
      return inputEmpty ? "hint" : "clear"
    },
    armed,
    disarm(): void { armedAt = undefined },
  }
}

export type QuitGuard = ReturnType<typeof createQuitGuard>
