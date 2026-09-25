import type { InputRenderable, KeyEvent, PasteEvent } from "@opentui/core"
import { useKeyboard, usePaste } from "@opentui/solid"
import { useApp } from "../app/context"
import { resolveBinding } from "../keys/bindings"
import { colors } from "../theme"

/**
 * Bordered prompt input. During `/key set` it swaps the input for a masked
 * `Key: •••` line and routes every key and paste to the controller's secret
 * entry, so the key never enters the input, the store, or the screen.
 */
export function Composer() {
  const { store, controller } = useApp()
  let input: InputRenderable | undefined
  let choices: string[] = []
  let index = -1
  let completed = ""
  const entering = () => store.state.secretProvider !== undefined

  function complete(): void {
    if (!input) return
    if (completed !== input.value) {
      choices = controller.complete(input.value)
      index = -1
    }
    if (!choices.length) return
    index = (index + 1) % choices.length
    input.value = choices[index] ?? input.value
    completed = input.value
    store.setStatus(`${index + 1}/${choices.length} completion · Tab cycles`)
  }

  useKeyboard((key: KeyEvent) => {
    if (entering()) {
      key.preventDefault()
      key.stopPropagation()
      controller.secretKey(key)
      return
    }
    switch (resolveBinding(key)) {
      case "complete":
        key.preventDefault()
        key.stopPropagation()
        complete()
        return
      case "refresh":
        controller.refreshAll()
        return
    }
  })

  usePaste((event: PasteEvent) => {
    if (!entering()) return
    event.preventDefault()
    event.stopPropagation()
    controller.secretPaste(new TextDecoder().decode(event.bytes))
  })

  return (
    <box height={3} border borderColor={colors.border} backgroundColor={colors.panel} paddingX={1}>
      <input
        ref={(element) => (input = element)}
        width="100%"
        maxLength={10_000}
        placeholder="Message or /command"
        textColor={colors.fg}
        cursorColor={colors.accent}
        visible={!entering()}
        focused={!entering()}
        // The typed prop also admits a DOM SubmitEvent; OpenTUI passes the line.
        onSubmit={(value: unknown) => {
          if (input) input.value = ""
          void controller.submit(String(value))
        }}
        onInput={(value: string) => {
          if (!value.startsWith("/")) return
          const found = controller.complete(value)
          if (found.length) store.setStatus(`Tab: ${found.slice(0, 5).join("  ")}${found.length > 5 ? "  …" : ""}`)
        }}
      />
      <text width="100%" fg={colors.accent} visible={entering()}>{entering() ? `Key: ${store.state.secretMask}` : ""}</text>
    </box>
  )
}
