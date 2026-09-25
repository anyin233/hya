/**
 * One shared spinner clock for running tool cards and subagent statuses: the
 * frame advances every 100 ms while at least one `<Spinner>` is mounted, and
 * the timer stops when the last one unmounts.
 */
import { createSignal, onCleanup } from "solid-js"

export const spinnerFrames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] as const
const frameMs = 100

const [frame, setFrame] = createSignal(0)
let users = 0
let timer: ReturnType<typeof setInterval> | undefined

function retain(): void {
  if (users++ === 0) timer = setInterval(() => setFrame((value) => (value + 1) % spinnerFrames.length), frameMs)
}

function release(): void {
  if (--users === 0 && timer) {
    clearInterval(timer)
    timer = undefined
  }
}

/** The current spinner glyph; keeps the clock running while the calling component is mounted. */
export function useSpinner(): () => string {
  retain()
  onCleanup(release)
  return () => spinnerFrames[frame()]!
}
