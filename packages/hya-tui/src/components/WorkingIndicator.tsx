/**
 * E21 working indicator (docs/tui.md "Working indicator"): while a turn this
 * client admitted runs, one line shows the spinner, elapsed time (mm:ss),
 * and the current activity (`state/activity.ts`) — `Thinking…`, `Writing…`,
 * `Running <tool> <summary>`, `Waiting for approval` / `Waiting for an
 * answer`, or `Waiting for subagent <agent>` — plus `Queued N` and an
 * `Esc to interrupt` hint. Placed below the transcript, above the pending
 * block and the permission/question prompt dock (App.tsx): the line that
 * belongs to the run in progress sits closest to the transcript it narrates,
 * and the dock below it still gets the last word when a prompt is pending.
 */
import { createSignal, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { workingLineText } from "../state/activity"
import { colors } from "../theme"
import { useSpinner } from "./Spinner"

/** Ticks once a second so the elapsed time stays current while a turn runs. */
function useNow(intervalMs = 1000): () => number {
  const [now, setNow] = createSignal(Date.now())
  const timer = setInterval(() => setNow(Date.now()), intervalMs)
  onCleanup(() => clearInterval(timer))
  return now
}

export function WorkingIndicator() {
  const { store } = useApp()
  const frame = useSpinner()
  const now = useNow()
  const text = () => workingLineText(store.state, now())
  return (
    <Show when={text()}>
      {(line) => (
        <text height={1} wrapMode="none">
          <span style={{ fg: colors.accent }}>{frame()}</span>
          <span style={{ fg: colors.muted }}>{` ${line()}`}</span>
        </text>
      )}
    </Show>
  )
}
