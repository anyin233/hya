import type { InputRenderable, KeyEvent } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js"
import { useApp, type CommandPaneHandle } from "../app/context"
import { historyEntry } from "../bridge"
import { commandSuggestionLimit, suggestCommandInput, type CommandSuggestion } from "../commands"
import { InputHistory } from "../composer/history"
import { projectsSidebarVisible } from "../state/layout"
import { pickerWindow } from "../state/picker"
import { isShiftTab } from "../state/modes"
import { keyboardOwner } from "../state/focus"
import { colors } from "../theme"

interface CommandMenu {
  items: CommandSuggestion[]
  index: number
  scope: string
}

/** A single command input that owns slash completion and command history. */
export function CommandPane() {
  const { store, controller, ui } = useApp()
  const size = useTerminalDimensions()
  const width = () => Math.max(20, Math.min(96, size().width - 4))
  const left = () => Math.max(0, Math.floor((size().width - width()) / 2))
  let editor: InputRenderable | undefined
  const [active, setActive] = createSignal(ui.commandInput?.active ?? false)
  const [menu, setMenu] = createSignal<CommandMenu | undefined>()
  const visibleMenu = createMemo(() => {
    const shown = menu()
    if (!shown) return undefined
    const rows = Math.max(1, Math.min(commandSuggestionLimit, size().height - 6))
    const { start, end } = pickerWindow(shown.items.length, shown.index, rows)
    return { items: shown.items.slice(start, end), index: shown.index - start }
  })
  const history = ui.commandHistory ??= new InputHistory()
  let originSidebar = ui.commandInput?.originSidebar ?? false
  let replaced: string | undefined

  function replace(text: string): void {
    if (!editor) return
    replaced = text
    editor.setText(text)
    editor.cursorOffset = text.length
    ui.commandInput = { text: editor.plainText, active: active(), originSidebar }
    updateMenu()
  }

  function updateMenu(): void {
    if (!active() || !editor) return setMenu(undefined)
    const text = editor.plainText
    const items = suggestCommandInput(text, controller.commandEntries, controller.complete)
    const previous = menu()
    const scope = text.slice(0, text.lastIndexOf(" ") + 1)
    setMenu(items.length ? { items, index: previous?.scope === scope ? Math.min(previous.index, items.length - 1) : 0, scope } : undefined)
  }

  function sync(): void {
    if (!editor) return
    if (editor.plainText !== replaced) history.reset()
    replaced = undefined
    ui.commandInput = { text: editor.plainText, active: active(), originSidebar }
    updateMenu()
  }

  function close(): void {
    if (editor) {
      const safe = historyEntry(editor.plainText)
      if (safe !== editor.plainText) replace(safe)
    }
    setActive(false)
    ui.commandInput = { text: editor?.plainText ?? "", active: false, originSidebar }
    setMenu(undefined)
    if (originSidebar && projectsSidebarVisible(store.state.projectsSidebar, store.state.columns)) store.setProjectsSidebarFocus(true)
  }

  function open(): void {
    if (active()) return
    originSidebar = store.state.projectsSidebarFocus
    ui.commandInput = { text: editor?.plainText ?? "", active: true, originSidebar }
    store.setProjectsSidebarFocus(false)
    setActive(true)
    if (!editor?.plainText.startsWith("/")) replace("/")
    else updateMenu()
  }

  function submit(): void {
    if (!active() || !editor) return
    const text = editor.plainText.trim()
    if (text.length < 2 || !text.startsWith("/")) return
    history.push(historyEntry(text))
    close()
    replace("")
    void controller.submit(text, "command")
  }

  function acceptEntry(run: boolean): void {
    const shown = menu()
    const entry = shown?.items[shown.index]
    if (!entry) return
    if (run && entry.runOnEnter) {
      replace(entry.replacement)
      submit()
      return
    }
    replace(`${entry.replacement} `)
  }

  function key(event: KeyEvent): boolean {
    if (!active()) return false
    // onContentChange can arrive after fast typing; always read the current
    // input before acting on the highlighted command.
    updateMenu()
    if (event.name === "escape" && !event.ctrl && !event.meta) {
      close()
      return true
    }
    if (event.name === "c" && event.ctrl && !event.meta) {
      close()
      return true
    }
    if (event.name === "backspace" && !event.ctrl && !event.meta && editor) {
      editor.deleteCharBackward()
      if (!editor.plainText) close()
      return true
    }
    const shown = menu()
    if (event.shift && !event.ctrl && !event.meta && (event.name === "up" || event.name === "down") && editor) {
      const text = event.name === "up" ? history.previous(editor.plainText) : history.next()
      if (text !== undefined) replace(text)
      return true
    }
    if (isShiftTab(event) && shown) {
      setMenu({ ...shown, index: (shown.index - 1 + shown.items.length) % shown.items.length })
      return true
    }
    if (!event.ctrl && !event.meta && (event.name === "up" || event.name === "down")) {
      if (shown) {
        const step = event.name === "up" ? -1 : 1
        setMenu({ ...shown, index: (shown.index + step + shown.items.length) % shown.items.length })
      } else if (editor) {
        const text = event.name === "up" ? history.previous(editor.plainText) : history.next()
        if (text !== undefined) replace(text)
      }
      return true
    }
    if (!event.ctrl && !event.meta && event.name === "tab") {
      if (shown) acceptEntry(false)
      return true
    }
    if (!event.ctrl && !event.meta && !event.shift && (event.name === "return" || event.name === "kpenter")) {
      if (/^\/keybind\s+show\s*$/.test(editor?.plainText ?? "")) submit()
      else if (shown) acceptEntry(true)
      else submit()
      return true
    }
    return false
  }

  const handle: CommandPaneHandle = {
    active,
    open,
    key,
    paste: (text) => editor?.insertText(text.replace(/\r?\n/g, " ")),
  }
  ui.command = handle
  onCleanup(() => {
    if (editor) ui.commandInput = { text: editor.plainText, active: active(), originSidebar }
    if (ui.command === handle) ui.command = undefined
  })

  return (
    <box position="absolute" top={2} left={left()} width={width()} zIndex={90} flexShrink={0} border borderColor={keyboardOwner(store.state, active()) === "commands" ? colors.accent : colors.border} title="Commands" backgroundColor={colors.panel} flexDirection="column" paddingX={1} visible={active()}>
      <input
        ref={(element: InputRenderable) => {
          editor = element
          if (ui.commandInput?.text) element.setText(ui.commandInput.text)
          element.cursorOffset = element.plainText.length
          updateMenu()
        }}
        width="100%"
        placeholder="/command [arguments]"
        textColor={colors.fg}
        focusedTextColor={colors.fg}
        cursorColor={colors.accent}
        focused={keyboardOwner(store.state, active()) === "commands"}
        onSubmit={submit}
        onContentChange={sync}
      />
      <Show when={visibleMenu()}>
        {(shown) => (
          <For each={shown().items}>
            {(entry, row) => (
              <text height={1} wrapMode="none" fg={row() === shown().index ? colors.accent : colors.fg}>
                {`${row() === shown().index ? "▸" : " "} ${entry.label}`}
              </text>
            )}
          </For>
        )}
      </Show>
      <text height={1} wrapMode="none" fg={colors.muted}>Up/Down select · Shift+Up/Down history · Tab chooses · Enter chooses/runs · Esc returns</text>
    </box>
  )
}
