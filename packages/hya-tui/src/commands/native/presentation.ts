import { copyNotice } from "../../composer/clipboard"
import { layoutBreakpoints, parseSwitch, sidebarTooNarrowNotice, sidebarVisible } from "../../state/layout"
import { lastReplyText, transcriptViews } from "../../state/messages"
import { autoThemeFor, autoThemeSetting, resolveThemeSetting, setTheme, setThemeSetting, terminalThemeMode, themeSetting, themes, type ThemeDefinition } from "../../theme"
import { matchValues, type CommandContext, type CommandSpec } from "../registry"
import { enableSidebar } from "../config"

/**
 * Open the `/theme` picker: one row per built-in theme (tag `dark`/`light`),
 * the theme in effect marked. Moving the highlight previews a theme; Enter
 * keeps it and saves it as `theme` in the preferences file (src/prefs.ts);
 * Esc restores the theme in effect when the picker opened.
 */
function openThemePicker({ store, actions }: CommandContext): void {
  const previous = themeSetting()
  const mode = terminalThemeMode()
  const auto = { id: autoThemeSetting, label: "Auto", tag: mode ?? "auto", detail: `Follow the terminal background: ${mode ? `${mode} now, so ${themes[autoThemeFor(mode)].label}` : "not reported yet, so hya"} (light → Light, dark → hya)`, current: previous === autoThemeSetting }
  actions.openPicker({
    title: "Theme",
    rows: [auto, ...(Object.values(themes) as ThemeDefinition[]).map((theme) => ({
      id: theme.name, label: theme.label, tag: theme.kind, detail: theme.description, current: theme.name === previous,
    }))],
    hint: "↑↓ previews · Enter keeps and saves · Esc restores · type to filter",
    onHighlight: (row) => { setTheme(resolveThemeSetting(row.id)) },
    onCancel: () => { setThemeSetting(previous) },
    onSelect: (row) => {
      if (!setThemeSetting(row.id)) throw new Error(`Unknown theme ${row.id}`)
      try {
        actions.savePreferences({ theme: row.id })
        store.setStatus(`Theme → ${row.label}`)
      } catch (error) {
        store.setStatus(`Theme → ${row.label} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  })
}

const switchValues = ["on", "off"]

export const helpCommand: CommandSpec = {
  name: "/help",
  description: "Show every key and command in a filterable overlay (also ? on an empty input)",
  run: ({ actions }) => { actions.openHelp() },
}

export const presentationCommands: CommandSpec[] = [
  {
    name: "/sidebar",
    description: "Show or hide the sidebar",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store, actions }, { args }) => {
      const component = store.state.structure.sidebar.enabled
      const shown = parseSwitch(args[0], component && sidebarVisible(store.state.sidebar, store.state.columns), "Usage: /sidebar [on|off]")
      // A single-column preset: `on` turns the sidebar component on for this session (`/set sidebar on`).
      if (shown && !component) enableSidebar(actions)
      if (shown && store.state.columns < layoutBreakpoints.sidebar) {
        store.setStatus(sidebarTooNarrowNotice)
        return
      }
      store.setSidebar(shown ? "auto" : "closed")
      store.setStatus(`Sidebar ${shown ? "shown" : "hidden"}`)
    },
  },
  {
    name: "/thinking",
    description: "Expand or collapse reasoning blocks",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store }, { args }) => {
      const expanded = parseSwitch(args[0], store.state.thinking, "Usage: /thinking [on|off]")
      store.setThinking(expanded)
      store.setStatus(`Reasoning ${expanded ? "expanded" : "collapsed"}`)
    },
  },
  {
    name: "/tools",
    description: "Expand or collapse tool call cards",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store }, { args }) => {
      const expanded = parseSwitch(args[0], store.state.tools ?? false, "Usage: /tools [on|off]")
      store.setTools(expanded)
      store.setStatus(`Tool calls ${expanded ? "expanded" : "collapsed"}`)
    },
  },
  {
    name: "/theme",
    description: "Pick the color theme (live preview while moving; Enter keeps and saves it, Esc restores)",
    run: (context) => { openThemePicker(context) },
  },
  {
    name: "/copy",
    description: "Copy the last assistant reply's text to the clipboard (OSC 52)",
    run: ({ store, actions }) => {
      const text = lastReplyText(transcriptViews(store.state))
      if (text === undefined) {
        store.setStatus("Nothing to copy: no assistant reply yet")
        return
      }
      store.setStatus(copyNotice(text, actions.copyText(text)))
    },
  },
  {
    name: "/editor",
    description: "Edit the input in $VISUAL / $EDITOR (fallback vi); the text comes back into the input",
    run: ({ actions }) => { actions.openEditor() },
  },
  {
    name: "/vim",
    description: "Turn vim mode in the input on or off (saved); Esc switches to normal mode",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store, actions }, { args }) => {
      const on = parseSwitch(args[0], store.state.vim, "Usage: /vim [on|off]")
      store.setVim(on)
      const text = on ? "Vim mode on · Esc for normal mode, i to insert" : "Vim mode off"
      try {
        actions.savePreferences({ vim: on })
        store.setStatus(text)
      } catch (error) {
        store.setStatus(`${text} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  },
  {
    name: "/notifications",
    description: "Turn desktop notifications on or off (saved): a turn finishing, or a permission/question ask, while the terminal is unfocused",
    argumentHint: "[on|off]",
    complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, switchValues) : [],
    run: ({ store, actions }, { args }) => {
      const on = parseSwitch(args[0], store.state.notifications, "Usage: /notifications [on|off]")
      store.setNotifications(on)
      const text = `Desktop notifications ${on ? "on" : "off"}`
      try {
        actions.savePreferences({ notifications: on })
        store.setStatus(text)
      } catch (error) {
        store.setStatus(`${text} · not saved: ${error instanceof Error ? error.message : String(error)}`)
      }
    },
  },
]
