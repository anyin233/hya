import type { InputRenderable, KeyEvent } from "@opentui/core"
import { createSignal, For, onCleanup, Show } from "solid-js"
import { useApp, type CommandPaneHandle } from "../app/context"
import { historyEntry } from "../bridge"
import { commandSuggestionLimit, filterCommands, requiresArgument, type CommandEntry } from "../commands"
import { InputHistory } from "../composer/history"
import { isShiftTab } from "../state/modes"
import { colors } from "../theme"

interface CommandMenu {
  items: CommandEntry[]
  index: number
}

/** A single command input that owns slash completion and command history. */
export function CommandPane() {
  const { store, controller, ui } = useApp()
  let editor: InputRenderable | undefined
  const [active, setActive] = createSignal(false)
  const [menu, setMenu] = createSignal<CommandMenu | undefined>()
  const history = new InputHistory()
  let originSidebar = false
  let replaced: string | undefined
  let choices: string[] = []
  let index = -1
  let completed = ""

  function replace(text: string): void {
    if (!editor) return
    replaced = text
    editor.setText(text)
    editor.cursorOffset = text.length
    updateMenu()
  }

  function updateMenu(): void {
    if (!active() || !editor) return setMenu(undefined)
    const text = editor.plainText
    if (!text.startsWith("/") || /\s/.test(text)) return setMenu(undefined)
    const items = filterCommands(controller.commandEntries(), text.slice(1)).slice(0, commandSuggestionLimit)
    setMenu(items.length ? { items, index: Math.min(menu()?.index ?? 0, items.length - 1) } : undefined)
  }

  function sync(): void {
    if (!editor) return
    if (editor.plainText !== replaced) history.reset()
    replaced = undefined
    updateMenu()
  }

  function close(): void {
    if (editor) {
      const safe = historyEntry(editor.plainText)
      if (safe !== editor.plainText) replace(safe)
    }
    setActive(false)
    setMenu(undefined)
    if (originSidebar) store.setProjectsSidebarFocus(true)
  }

  function open(): void {
    if (active()) return
    originSidebar = store.state.projectsSidebarFocus
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
    const entry = menu()?.items[menu()?.index ?? 0]
    if (!entry) return
    if (run && !requiresArgument(entry.argumentHint)) {
      replace(entry.name)
      submit()
      return
    }
    replace(`${entry.name} `)
  }

  function complete(): void {
    if (!editor) return
    const text = editor.plainText
    if (completed !== text) {
      choices = controller.complete(text)
      index = -1
    }
    if (!choices.length) return
    index = (index + 1) % choices.length
    replace(choices[index] ?? text)
    completed = editor.plainText
    store.setStatus(`${index + 1}/${choices.length} completion · Tab cycles`)
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
      else complete()
      return true
    }
    if (!event.ctrl && !event.meta && !event.shift && (event.name === "return" || event.name === "kpenter")) {
      if (shown) acceptEntry(true)
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
  onCleanup(() => { if (ui.command === handle) ui.command = undefined })

  return (
    <box width="100%" flexShrink={0} border borderColor={colors.accent} title="Commands" backgroundColor={colors.panel} flexDirection="column" paddingX={1} visible={active()}>
      <Show when={menu()}>
        {(shown) => (
          <For each={shown().items}>
            {(entry, row) => (
              <text height={1} wrapMode="none" fg={row() === shown().index ? colors.accent : colors.fg}>
                {`${row() === shown().index ? "▸" : " "} ${entry.name}${entry.argumentHint ? ` ${entry.argumentHint}` : ""}  ${entry.description}  [${entry.source}]`}
              </text>
            )}
          </For>
        )}
      </Show>
      <input
        ref={(element: InputRenderable) => (editor = element)}
        width="100%"
        placeholder="/command [arguments]"
        textColor={colors.fg}
        focusedTextColor={colors.fg}
        cursorColor={colors.accent}
        focused={active() && !store.state.picker && !store.state.secretEntry}
        onSubmit={submit}
        onContentChange={sync}
      />
      <text height={1} wrapMode="none" fg={colors.muted}>Up/Down select · Shift+Up/Down history · Tab completes · Enter runs · Esc returns</text>
    </box>
  )
}
