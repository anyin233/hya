/** Keybinding inspection and persistent shortcuts to full slash commands. */
import { bindingScopes, bindingSettings, findBindingSetting, type BindingSetting } from "../keys/catalog"
import { customKeybindings, parseShortcut, setCustomKeybindings, validateCustomKeybindings, type CommandBindingScope, sameShortcut, type CustomKeybindings } from "../keys/custom"
import { activeInheritedBindingRows, inheritedBindingRows } from "../keys/inventory"
import type { PickerRow } from "../state/picker"
import { matchValues, type CommandContext, type CommandSpec } from "./registry"

const usage = "Usage: /keybind [list [scope] | show <action, command or shortcut> | set <shortcut> [--scope workspace|conversation] <command...> | unset <shortcut> | reset <shortcut|all>]"
const columns = { shortcut: "Shortcut", label: "Action / command", tag: "Scope" }
const hint = "↑↓ select · type to filter · Enter details · Esc closes"

function settingRow(setting: BindingSetting): PickerRow {
  return {
    id: setting.id,
    label: setting.id,
    tag: setting.scope,
    shortcut: setting.keys.join(" / ") || "unassigned",
    detail: `${setting.command ? `Command: ${setting.command}. ` : ""}${setting.context} ${setting.description}`,
  }
}

function openDetails(context: CommandContext, setting: BindingSetting): void {
  context.actions.openPicker({
    title: `Keybinding · ${setting.id}`,
    rows: [settingRow(setting)],
    detailPane: true, columns,
    maxRows: 1,
    hint: "Inspection only · Esc closes",
    onSelect: () => undefined,
  })
}

function customRows(): PickerRow[] {
  return Object.entries(customKeybindings()).filter((entry): entry is [string, NonNullable<typeof entry[1]>] => entry[1] !== null).map(([key, binding]) => ({
    id: `custom:${key}`, label: binding.command, shortcut: key, tag: binding.scope,
    detail: `Command: ${binding.command}. Custom shortcut (${binding.scope}); modals and command input take precedence.`,
  }))
}

function openCustom(context: CommandContext, row: PickerRow, saved = false): void {
  context.actions.openPicker({
    title: saved ? `Keybind saved · ${row.shortcut}` : `Keybinding · ${row.shortcut}`,
    rows: [row], detailPane: true, columns, maxRows: 1,
    hint: "Esc closes · unset disables · reset restores defaults", onSelect: () => undefined,
  })
}

function fail(context: CommandContext, message: string): never {
  context.actions.openPicker({ title: "Keybind · error", rows: [{ id: "error", label: "Error", detail: message }], detailPane: true, onSelect: () => undefined })
  throw new Error(message)
}

function disabledRow(key: string): PickerRow {
  return { id: `disabled:${key}`, label: "disabled", shortcut: key, tag: "all contexts", detail: "Disabled by /keybind unset. /keybind reset restores the default. Command input retains ownership so settings can be repaired." }
}

/** Save before changing runtime state, so failed writes leave bindings untouched. */
function saveBindings(context: CommandContext, next: CustomKeybindings): void {
  const validated = validateCustomKeybindings(next)
  context.actions.savePreferences({ keybindings: validated })
  setCustomKeybindings(validated)
}

export const keybindingsCommand: CommandSpec = {
  name: "/keybind",
  description: "Browse keybindings or save shortcuts for full commands with arguments",
  argumentHint: "[list | show <target> | set <key> <command...> | unset <key> | reset <key|all>]",
  complete: ({ words, current, head }, context, registry) => {
    if (words.length === 1) return matchValues(head, current, ["list", "reset", "set", "show", "unset"])
    if (words.length === 2 && words[0] === "list") return matchValues(head, current, [...bindingScopes])
    if (words.length === 2 && words[0] === "show") {
      return matchValues(head, current, [...Object.keys(customKeybindings()), ...inheritedBindingRows().map((row) => row.shortcut!), ...bindingSettings().flatMap((setting) => [setting.id, ...(setting.command ? [setting.command] : [])])])
    }
    if (words.length === 2 && words[0] === "reset") return matchValues(head, current, ["all", ...Object.keys(customKeybindings())])
    if (words.length === 2 && words[0] === "unset") return matchValues(head, current, [...Object.keys(customKeybindings()), ...bindingSettings().flatMap((entry) => entry.keys.map((key) => key.startsWith("Ctrl+X ") ? "Ctrl+X" : key.replace(/ \(empty input\)$/, ""))), ...activeInheritedBindingRows().map((row) => row.shortcut!)])
    if (words[0] === "set") {
      if (words.length === 2) return matchValues(head, current, ["F5", "F6", "F7", "F8", "F9", "F10", "Alt+G", "Alt+K"])
      if (words.length === 3) return matchValues(head, current, ["--scope", ...(registry?.names() ?? []), ...context.backendCommands.map((name) => `/${name}`)])
      if (words.length === 4 && words[2] === "--scope") return matchValues(head, current, ["workspace", "conversation"])
      if (words.length === 5 && words[2] === "--scope") return matchValues(head, current, [...(registry?.names() ?? []), ...context.backendCommands.map((name) => `/${name}`)])
      const prefix = /^\/keybind\s+set\s+\S+\s+(?:--scope\s+\S+\s+)?/.exec(head)?.[0]
      if (prefix && registry) {
        return registry.complete(head.slice(prefix.length) + current, context).map((choice) => typeof choice === "string"
          ? prefix + choice
          : { replacement: prefix + choice.replacement, label: prefix + choice.label })
      }
    }
    return []
  },
  run: (context, { args, argumentsText }) => {
    if (args[0] === "set" || args[0] === "reset" || args[0] === "unset") {
      try {
        const next = customKeybindings()
        if (args[0] === "reset" || args[0] === "unset") {
          if (args.length !== 2) throw new Error(usage)
          if (args[0] === "reset" && args[1] === "all") saveBindings(context, {})
          else {
            const key = parseShortcut(args[1]!).label
            if (args[0] === "unset") {
              for (const previous of Object.keys(next)) if (previous !== key && sameShortcut(previous, key)) delete next[previous]
              next[key] = null
            }
            else {
              if (!Object.hasOwn(next, key)) throw new Error(`No override for ${key}`)
              delete next[key]
            }
            saveBindings(context, next)
          }
          context.actions.openPicker({ title: args[0] === "unset" ? "Keybind unset" : "Keybind reset", rows: [{ id: "removed", label: args[1]!, detail: args[0] === "unset" ? "Shortcut disabled. /keybind reset restores its default." : "Override removed; default behavior restored." }], detailPane: true, onSelect: () => undefined })
          return
        }
        const parsed = /^set\s+(\S+)\s+(?:--scope\s+(\S+)\s+)?([\s\S]+)$/.exec(argumentsText)
        if (!parsed) throw new Error(usage)
        const key = parseShortcut(parsed[1]!).label
        const command = parsed[3]!
        const scope = parsed[2] ?? bindingSettings().find((setting) => setting.command?.split(" ")[0] === command.split(" ")[0])?.scope ?? "conversation"
        if (scope !== "workspace" && scope !== "conversation") throw new Error("Custom binding scope must be workspace or conversation")
        for (const previous of Object.keys(next)) if (previous !== key && sameShortcut(previous, key)) delete next[previous]
        next[key] = { command, scope: scope as CommandBindingScope }
        saveBindings(context, next)
        openCustom(context, customRows().find((row) => row.shortcut === key)!, true)
      } catch (error) {
        context.actions.openPicker({ title: "Keybind · not saved", rows: [{ id: "error", label: "Not saved", detail: error instanceof Error ? error.message : String(error) }], detailPane: true, onSelect: () => undefined })
        throw error
      }
      return
    }
    if (args[0] === "show") {
      if (args.length < 2) fail(context, "Usage: /keybind show <shortcut, action or command>. Example: /keybind show Ctrl+W")
      const target = args.slice(1).join(" ")
      let shortcut = target
      try { shortcut = parseShortcut(target).label } catch { /* Action IDs and commands are not shortcut labels. */ }
      const saved = customKeybindings()
      if (Object.hasOwn(saved, shortcut) && saved[shortcut] === null) { openCustom(context, disabledRow(shortcut)); return }
      const custom = customRows().find((row) => row.shortcut === shortcut || row.detail?.startsWith(`Command: ${target}.`))
      if (custom) { openCustom(context, custom); return }
      const matches = inheritedBindingRows().filter((row) => row.shortcut === shortcut || row.id === target)
      const setting = findBindingSetting(target) ?? bindingSettings().find((entry) => entry.keys.some((key) => {
        try { return sameShortcut(key.startsWith("Ctrl+X ") ? "Ctrl+X" : key.replace(/ \(empty input\)$/, ""), shortcut) } catch { return false }
      }))
      if (setting && !matches.length) { openDetails(context, setting); return }
      if (matches.length) {
        context.actions.openPicker({ title: `Keybinding · ${shortcut}`, rows: [...(setting ? [settingRow(setting)] : []), ...matches], columns, detailPane: true, onSelect: () => undefined })
        return
      }
      if (!setting) fail(context, `No binding for ${target}. Use /keybind list to browse assigned shortcuts.`)
      openDetails(context, setting)
      return
    }
    if (args.length && args[0] !== "list") throw new Error(usage)
    if (args.length > 2 || (args[1] && !bindingScopes.some((scope) => scope === args[1]))) throw new Error(usage)
    const scope = args[1]
    context.actions.openPicker({
      title: `Keybindings${scope ? ` · ${scope}` : ""}`,
      rows: [...customRows(), ...activeInheritedBindingRows(), ...bindingSettings().filter((setting) => setting.keys.length > 0).map(settingRow)].filter((row) => !scope || row.tag === scope),
      detailPane: true, columns,
      maxRows: 8,
      hint,
      onSelect: (row) => {
        if (row.id.startsWith("custom:")) { openCustom(context, row); return }
        if (row.id.startsWith("editor:") || row.id.startsWith("context:")) {
          context.actions.openPicker({ title: `Keybinding · ${row.shortcut}`, rows: [row], columns, detailPane: true, onSelect: () => undefined }); return
        }
        const setting = findBindingSetting(row.id)
        if (setting) openDetails(context, setting)
      },
    })
  },
}
