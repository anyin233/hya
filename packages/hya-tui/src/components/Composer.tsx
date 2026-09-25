import type { KeyEvent, PasteEvent, TextareaRenderable } from "@opentui/core"
import { useKeyboard, usePaste, useTerminalDimensions } from "@opentui/solid"
import { createEffect, createSignal, For, onCleanup, Show } from "solid-js"
import { useApp } from "../app/context"
import { readOnlyStatus } from "../app/controller"
import { commandSuggestionLimit, filterCommands, requiresArgument, type CommandEntry } from "../commands"
import { escapeAction } from "../composer/escape"
import { InputHistory } from "../composer/history"
import { insertMention, mentionAt, type MentionToken } from "../composer/mention"
import { createQuitGuard, quitWindowMs } from "../composer/quit"
import { isShellInput } from "../composer/shell"
import { composerKeyBindings, resolveBinding } from "../keys/bindings"
import { currentPrompt, promptKey } from "../state/prompts"
import { colors } from "../theme"

/** Most input rows before the input scrolls. */
export const composerMaxRows = 8
/** Quiet time before an `@file` lookup is sent. */
const mentionDebounceMs = 120
export const quitHint = "Press Ctrl+C again to quit"

interface FileMenu {
  token: MentionToken
  items: string[]
  index: number
}

interface CmdMenu {
  items: CommandEntry[]
  index: number
}

/**
 * Bordered multi-line prompt editor (OpenTUI `<textarea>`). Enter submits;
 * Ctrl+J / Alt+Enter (and Shift+Enter where the terminal reports it) insert a
 * newline; the box grows with its content up to `composerMaxRows` rows, then
 * scrolls. Up/Down on the first/last line walk the input history. Esc closes
 * the `@file` list, else declines a shown prompt, else cancels the running
 * turn, else clears the input (composer/escape.ts).
 *
 * Permission/question prompts (components/PromptDock.tsx): after the lists,
 * a shown prompt takes digits, Up/Down, Enter, and Esc while the input is
 * empty; with text in the input a question takes Enter as its answer
 * (state/prompts.ts `promptKey`). Other keys always reach the editor.
 * Ctrl+C clears (or hints) and quits on a second press within 2 s; Ctrl+D
 * quits on an empty input. `!command` input shows a shell-mode border; an
 * `@text` token opens a file suggestion list from `FindFiles`.
 *
 * During `/key set` it swaps the editor for a masked `Key: •••` line and
 * routes every key and paste to the controller's secret entry, so the key
 * never enters the editor, the store, or the screen.
 */
export function Composer() {
  const { store, controller, ui } = useApp()
  const size = useTerminalDimensions()
  let editor: TextareaRenderable | undefined
  const [value, setValue] = createSignal("")
  const [rows, setRows] = createSignal(1)
  const [menu, setMenu] = createSignal<FileMenu | undefined>()
  const [cmdMenu, setCmdMenu] = createSignal<CmdMenu | undefined>()
  const history = new InputHistory()
  const quitGuard = createQuitGuard()
  let choices: string[] = []
  let index = -1
  let completed = ""
  /**
   * Text the composer itself last put in the editor (history, completion,
   * clearing). Content-change events arrive after the change, so a change
   * to exactly this text is not a user edit.
   */
  let replaced: string | undefined
  /** The `@` token (`start:query`) the user closed with Esc; editing the token opens the list again. */
  let dismissed: string | undefined
  let lookupTimer: ReturnType<typeof setTimeout> | undefined
  let lookupTicket = 0
  let hintTimer: ReturnType<typeof setTimeout> | undefined
  const entering = () => store.state.secretProvider !== undefined
  /** A subagent's session is open: prompts are disabled, slash commands still run. */
  const readOnly = () => Boolean(store.state.selected?.parent)
  const shell = () => isShellInput(value())

  onCleanup(() => {
    if (lookupTimer) clearTimeout(lookupTimer)
    if (hintTimer) clearTimeout(hintTimer)
  })

  /** Rows the text needs at the editor's width (wrapped lines counted), capped at `composerMaxRows`. */
  function measure(): void {
    if (!editor) return
    const width = Math.max(1, editor.width)
    const lines = editor.plainText.split("\n")
      .reduce((total, line) => total + Math.max(1, Math.ceil((Bun.stringWidth(line) + 1) / width)), 0)
    setRows(Math.max(1, Math.min(composerMaxRows, lines)))
  }

  // Wrapping changes with the width.
  createEffect(() => {
    size()
    queueMicrotask(measure)
  })

  /** Replace the whole input and put the cursor at `cursor` (default: the end). */
  function replace(text: string, cursor = text.length): void {
    if (!editor) return
    replaced = text
    editor.setText(text)
    editor.cursorOffset = cursor
    sync()
  }

  function closeMenu(): void {
    if (lookupTimer) clearTimeout(lookupTimer)
    lookupTimer = undefined
    lookupTicket++
    setMenu(undefined)
  }

  function closeCmdMenu(): void { setCmdMenu(undefined) }

  /**
   * Open, refresh, or close the `/` command menu: shown while the whole
   * input is `/` plus a name being typed (no space yet — once a space is
   * typed the name is settled and argument completion takes over via the
   * existing Tab `complete()` cycle). Fuzzy-filters the merged local +
   * backend (commands and skills) command list; see commands/menu.ts.
   */
  function updateCommandMenu(): void {
    if (!editor || entering()) return closeCmdMenu()
    const text = editor.plainText
    if (!text.startsWith("/") || /\s/.test(text)) return closeCmdMenu()
    const items = filterCommands(controller.commandEntries(), text.slice(1)).slice(0, commandSuggestionLimit)
    if (!items.length) return closeCmdMenu()
    setCmdMenu((open) => ({ items, index: open && open.index < items.length ? open.index : 0 }))
  }

  /** Tab: complete the highlighted command's name (keeps typing args). Enter: run it if it needs no argument, else complete like Tab (commands/menu.ts `requiresArgument`). */
  function acceptCommandEntry(run: boolean): void {
    const open = cmdMenu()
    const entry = open?.items[open.index]
    if (!editor || !entry) return
    if (run && !requiresArgument(entry.argumentHint)) {
      closeCmdMenu()
      replace(entry.name)
      submit()
      return
    }
    closeCmdMenu()
    replace(`${entry.name} `)
  }

  /** Open, refresh, or close the `@file` list for the token at the cursor. */
  function updateMention(): void {
    if (!editor || entering()) return closeMenu()
    if (editor.plainText.startsWith("/")) return closeMenu()
    const token = mentionAt(editor.plainText, editor.cursorOffset)
    if (!token) {
      dismissed = undefined
      return closeMenu()
    }
    const key = `${token.start}:${token.query}`
    if (dismissed === key) return closeMenu()
    dismissed = undefined
    const open = menu()
    if (open && open.token.query === token.query && open.token.start === token.start) {
      setMenu({ ...open, token })
      return
    }
    if (open) setMenu({ ...open, token })
    if (lookupTimer) clearTimeout(lookupTimer)
    const ticket = ++lookupTicket
    lookupTimer = setTimeout(() => {
      lookupTimer = undefined
      void controller.findFiles(token.query)
        .then((items) => {
          if (ticket !== lookupTicket) return
          setMenu(items.length ? { token, items, index: 0 } : undefined)
        })
        .catch((error: unknown) => {
          if (ticket === lookupTicket) store.setStatus(`File lookup failed: ${String(error)}`)
        })
    }, mentionDebounceMs)
  }

  function acceptMention(): void {
    const open = menu()
    const path = open?.items[open.index]
    if (!editor || !open || path === undefined) return
    const next = insertMention(editor.plainText, open.token, path)
    closeMenu()
    replace(next.text, next.cursor)
  }

  /** Mirror the editor text into the signal; runs after every content change. */
  function sync(): void {
    if (!editor) return
    const text = editor.plainText
    setValue(text)
    store.setDraft(text.length > 0)
    measure()
    if (text !== replaced) {
      replaced = undefined
      history.reset()
      if (text.startsWith("/") && !text.includes("\n")) {
        const found = controller.complete(text)
        if (found.length) store.setStatus(`Tab: ${found.slice(0, 5).join("  ")}${found.length > 5 ? "  …" : ""}`)
      }
    }
    updateCommandMenu()
    updateMention()
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

  function submit(): void {
    if (!editor || entering()) return
    const text = editor.plainText
    if (!text.trim()) return
    if (readOnly() && !text.trim().startsWith("/")) {
      // Keep the text: it can be sent once back in the parent.
      store.setStatus(readOnlyStatus)
      return
    }
    history.push(text)
    closeMenu()
    closeCmdMenu()
    replace("")
    void controller.submit(text)
  }

  /** Status before the quit hint; restored when the hint expires unchanged. */
  let beforeHint = ""

  function showQuitHint(): void {
    if (store.state.status !== quitHint) beforeHint = store.state.status
    store.setStatus(quitHint)
    if (hintTimer) clearTimeout(hintTimer)
    hintTimer = setTimeout(() => {
      hintTimer = undefined
      if (store.state.status === quitHint) store.setStatus(beforeHint)
    }, quitWindowMs)
  }

  /** Up/Down: move in the command or file list, else walk history from the first/last line. */
  function arrow(key: KeyEvent, consume: () => void): boolean {
    if (key.ctrl || key.meta || key.shift || (key.name !== "up" && key.name !== "down")) return false
    const step = key.name === "up" ? -1 : 1
    const openCmd = cmdMenu()
    if (openCmd) {
      consume()
      setCmdMenu({ ...openCmd, index: (openCmd.index + step + openCmd.items.length) % openCmd.items.length })
      return true
    }
    const open = menu()
    if (open) {
      consume()
      setMenu({ ...open, index: (open.index + step + open.items.length) % open.items.length })
      return true
    }
    if (!editor) return false
    const row = editor.logicalCursor.row
    if (key.name === "up" && row === 0) {
      const older = history.previous(editor.plainText)
      if (older === undefined) return false
      consume()
      replace(older)
      return true
    }
    if (key.name === "down" && row >= editor.lineCount - 1 && history.navigating) {
      const newer = history.next()
      if (newer === undefined) return false
      consume()
      replace(newer)
      return true
    }
    return false
  }

  useKeyboard((key: KeyEvent) => {
    if (entering()) {
      key.preventDefault()
      key.stopPropagation()
      controller.secretKey(key)
      return
    }
    const consume = (): void => {
      key.preventDefault()
      key.stopPropagation()
    }
    // The editor's own text is always live; `sync()` (onContentChange) can
    // lag one render behind fast typing (programmatic or a fast typist), so
    // recompute the command menu from `editor.plainText` right here before
    // acting on it — Tab/Enter/Up/Down must never act on a stale filtered
    // list from before the most recent keystroke.
    updateCommandMenu()
    const cmdOpen = cmdMenu()
    if (cmdOpen && !key.ctrl && !key.meta && !key.shift && (key.name === "tab" || key.name === "return" || key.name === "kpenter")) {
      consume()
      acceptCommandEntry(key.name !== "tab")
      return
    }
    const open = menu()
    if (open && !key.ctrl && !key.meta && !key.shift && (key.name === "tab" || key.name === "return" || key.name === "kpenter")) {
      consume()
      acceptMention()
      return
    }
    // A shown permission/question prompt comes next (after the lists): with
    // an empty input it takes digits, Up/Down, Enter, and Esc; with text, a
    // question takes Enter as its answer. Other keys reach the input.
    const shown = cmdOpen || open ? undefined : currentPrompt(store.state)
    if (shown) {
      const draft = editor?.plainText ?? value()
      const result = promptKey(shown.view, { index: store.promptIndex(shown.view.id), draft }, key)
      if (result.type !== "none") {
        consume()
        quitGuard.disarm()
        if (result.type === "move") store.setPromptIndex(shown.view.id, result.index)
        else {
          if (draft.trim()) {
            // The typed text was the answer: keep it in history, clear the input.
            history.push(draft)
            replace("")
          }
          controller.answer(shown.interaction, result.choice)
        }
        return
      }
    }
    if (arrow(key, consume)) return
    const action = resolveBinding(key, { composerEmpty: !value() })
    if (action !== "quit") quitGuard.disarm()
    if (!action) return
    const transcript = store.state.view === "chat" ? ui.transcript : undefined
    switch (action) {
      case "interrupt": {
        consume()
        const escape = escapeAction({ menuOpen: open !== undefined || cmdOpen !== undefined, running: store.state.running, inputEmpty: !value(), childView: readOnly(), prompt: shown !== undefined })
        if (escape === "closeMenu") {
          dismissed = open ? `${open.token.start}:${open.token.query}` : undefined
          closeMenu()
          closeCmdMenu()
        } else if (escape === "declinePrompt") {
          if (shown) controller.answer(shown.interaction, shown.view.kind === "question" ? { kind: "reject" } : { kind: "deny" })
        } else if (escape === "returnToParent") controller.returnToParent()
        else if (escape === "cancelTurn") controller.cancelTurn()
        else if (escape === "clearInput") replace("")
        return
      }
      case "quit": {
        consume()
        const outcome = quitGuard.press(!value())
        if (outcome === "quit") {
          controller.quit()
          return
        }
        if (outcome === "clear") {
          closeMenu()
          replace("")
        }
        showQuitHint()
        return
      }
      case "eof":
        consume()
        controller.quit()
        return
      case "complete":
        consume()
        complete()
        return
      case "refresh":
        controller.refreshAll()
        return
      case "toggleSidebar":
        consume()
        store.toggleSidebar()
        return
      case "toggleThinking":
        consume()
        store.setThinking(!store.state.thinking)
        store.setStatus(`Reasoning ${store.state.thinking ? "expanded" : "collapsed"} · Ctrl+O toggles`)
        return
      case "toggleTools": {
        consume()
        const expanded = !(store.state.tools ?? false)
        store.setTools(expanded)
        store.setStatus(`Tool calls ${expanded ? "expanded" : "collapsed"} · Ctrl+G toggles`)
        return
      }
      case "pageUp":
      case "pageDown":
        consume()
        transcript?.page(action === "pageUp" ? -1 : 1)
        return
      case "scrollTop":
        consume()
        transcript?.top()
        return
      case "scrollBottom":
        consume()
        transcript?.bottom()
        return
    }
  })

  usePaste((event: PasteEvent) => {
    event.preventDefault()
    event.stopPropagation()
    const text = new TextDecoder().decode(event.bytes)
    if (entering()) {
      controller.secretPaste(text)
      return
    }
    // A bracketed paste never submits: its line breaks (CR from xterm.js) become newlines.
    // eslint-disable-next-line no-control-regex
    editor?.insertText(text.replace(/\x1b\[[0-9;]*[A-Za-z]/g, "").replace(/\r\n?/g, "\n"))
  })

  return (
    <box width="100%" flexShrink={0} flexDirection="column">
      <Show when={cmdMenu()}>
        {(open) => (
          <box width="100%" flexShrink={0} border borderColor={colors.border} title="Commands" backgroundColor={colors.panel} flexDirection="column" paddingX={1}>
            <For each={open().items}>
              {(entry, row) => (
                <text height={1} wrapMode="none" fg={row() === open().index ? colors.accent : colors.fg}>
                  {`${row() === open().index ? "▸" : " "} ${entry.name}${entry.argumentHint ? ` ${entry.argumentHint}` : ""}  ${entry.description}  [${entry.source}]`}
                </text>
              )}
            </For>
            <text height={1} wrapMode="none" fg={colors.muted}>Up/Down select · Tab completes name · Enter runs or completes · Esc closes</text>
          </box>
        )}
      </Show>
      <Show when={menu()}>
        {(open) => (
          <box width="100%" flexShrink={0} border borderColor={colors.border} title="Files" backgroundColor={colors.panel} flexDirection="column" paddingX={1}>
            <For each={open().items}>
              {(path, row) => (
                <text height={1} wrapMode="none" fg={row() === open().index ? colors.accent : colors.fg}>
                  {`${row() === open().index ? "▸" : " "} ${path}`}
                </text>
              )}
            </For>
            <text height={1} wrapMode="none" fg={colors.muted}>Up/Down select · Tab/Enter insert · Esc closes</text>
          </box>
        )}
      </Show>
      <box
        width="100%"
        height={rows() + 2}
        flexShrink={0}
        border
        borderColor={shell() && !entering() ? colors.warning : colors.border}
        title={shell() && !entering() ? "! shell" : undefined}
        backgroundColor={colors.panel}
        paddingX={1}
      >
        <textarea
          ref={(element: TextareaRenderable) => (editor = element)}
          width="100%"
          height={rows()}
          placeholder={readOnly() ? "Read-only subagent view · /commands work · Esc returns" : "Message, /command, !shell, or @file"}
          textColor={colors.fg}
          focusedTextColor={colors.fg}
          cursorColor={colors.accent}
          wrapMode="word"
          keyBindings={[...composerKeyBindings]}
          visible={!entering()}
          focused={!entering()}
          onSubmit={submit}
          onContentChange={sync}
          onCursorChange={() => updateMention()}
        />
        <text width="100%" fg={colors.accent} visible={entering()}>{entering() ? `Key: ${store.state.secretMask}` : ""}</text>
      </box>
    </box>
  )
}
