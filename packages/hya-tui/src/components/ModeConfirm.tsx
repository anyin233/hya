/**
 * The one-line yolo confirmation (app/modes.ts, docs/tui.md "Permission
 * modes"): shown above the status line the first time this TUI process
 * switches to `yolo`. Enter confirms, Esc cancels, Shift+Tab skips to the
 * next mode; any other key cancels it. Keys are handled in
 * components/Composer.tsx before the permission prompt's own keys, so Enter
 * and Esc here never answer a pending ask.
 */
import { Show } from "solid-js"
import { useApp } from "../app/context"
import { yoloConfirmText } from "../state/modes"
import { colors } from "../theme"

export function ModeConfirm() {
  const { store } = useApp()
  return (
    <Show when={store.state.modeConfirm}>
      <text width="100%" height={1} flexShrink={0} wrapMode="none" bg={colors.panel}>
        <span style={{ fg: colors.error }}>{"⚠ "}</span>
        <b style={{ fg: colors.error }}>{yoloConfirmText.slice(0, yoloConfirmText.indexOf(" · "))}</b>
        <span style={{ fg: colors.fg }}>{yoloConfirmText.slice(yoloConfirmText.indexOf(" · "))}</span>
      </text>
    </Show>
  )
}
