/** Keybinding inspection and persistent shortcuts to full slash commands. */
import { bindingScopes, bindingSettings, findBindingSetting, type BindingSetting } from "../keys/catalog"
import { customKeybindings, parseShortcut, setCustomKeybindings, validateCustomKeybindings, type CommandBindingScope, type CustomKeybindings } from "../keys/custom"
import type { PickerRow } from "../state/picker"
import { matchValues, type CommandContext, type CommandSpec } from "./registry"

const usage = "Usage: /keybind [list [scope] | show <action, command or shortcut> | set <shortcut> [--scope workspace|conversation] <command...> | reset <shortcut|all>]"
const hint = "↑↓ select · type to filter · Enter details · Esc closes"

function settingRow(setting: BindingSetting): PickerRow {
  return {
    id: setting.id,
    label: setting.id,
    tag: setting.scope,
    detail: `Keys: ${setting.keys.join(" / ") || "unassigned"}. ${setting.command ? `Command: ${setting.command}. ` : ""}${setting.context} ${setting.description}`,
  }
}

function openDetails(context: CommandContext, setting: BindingSetting): void {
  context.actions.openPicker({
    title: `Keybinding · ${setting.id}`,
    rows: [settingRow(setting)],
    detailPane: true,
    maxRows: 1,
    hint: "Inspection only · Esc closes",
    onSelect: () => undefined,
  })
}

function customRows(): PickerRow[] {
  return Object.entries(customKeybindings()).map(([key, binding]) => ({
    id: `custom:${key}`, label: key, tag: binding.scope,
    detail: `Command: ${binding.command}. Custom shortcut (${binding.scope}); modals and command input take precedence.`,
  }))
}

function openCustom(context: CommandContext, row: PickerRow, saved = false): void {
  context.actions.openPicker({
    title: saved ? `Keybind saved · ${row.label}` : `Keybinding · ${row.label}`,
    rows: [row], detailPane: true, maxRows: 1,
    hint: "Esc closes · /keybind reset removes a custom shortcut", onSelect: () => undefined,
  })
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
  argumentHint: "[list | show <target> | set <key> <command...> | reset <key|all>]",
  complete: ({ words, current, head }, context, registry) => {
    if (words.length === 1) return matchValues(head, current, ["list", "reset", "set", "show"])
    if (words.length === 2 && words[0] === "list") return matchValues(head, current, [...bindingScopes])
    if (words.length === 2 && words[0] === "show") {
      return matchValues(head, current, [...Object.keys(customKeybindings()), ...bindingSettings().flatMap((setting) => [setting.id, ...(setting.command ? [setting.command] : [])])])
    }
    if (words.length === 2 && words[0] === "reset") return matchValues(head, current, ["all", ...Object.keys(customKeybindings())])
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
    if (args[0] === "set" || args[0] === "reset") {
      try {
        const next = customKeybindings()
        if (args[0] === "reset") {
          if (args.length !== 2) throw new Error(usage)
          if (args[1] === "all") saveBindings(context, {})
          else {
            const key = parseShortcut(args[1]!).label
            if (!next[key]) throw new Error(`No custom binding for ${key}`)
            delete next[key]
            saveBindings(context, next)
          }
          context.actions.openPicker({ title: "Keybind reset", rows: [{ id: "reset", label: args[1]!, detail: "Custom assignment removed. Built-in shortcuts remain available." }], detailPane: true, onSelect: () => undefined })
          return
        }
        const parsed = /^set\s+(\S+)\s+(?:--scope\s+(\S+)\s+)?([\s\S]+)$/.exec(argumentsText)
        if (!parsed) throw new Error(usage)
        const key = parseShortcut(parsed[1]!).label
        const command = parsed[3]!
        const scope = parsed[2] ?? bindingSettings().find((setting) => setting.command?.split(" ")[0] === command.split(" ")[0])?.scope ?? "conversation"
        if (scope !== "workspace" && scope !== "conversation") throw new Error("Custom binding scope must be workspace or conversation")
        next[key] = { command, scope: scope as CommandBindingScope }
        saveBindings(context, next)
        openCustom(context, customRows().find((row) => row.label === key)!, true)
      } catch (error) {
        context.actions.openPicker({ title: "Keybind · not saved", rows: [{ id: "error", label: "Not saved", detail: error instanceof Error ? error.message : String(error) }], detailPane: true, onSelect: () => undefined })
        throw error
      }
      return
    }
    if (args[0] === "show") {
      if (args.length < 2) throw new Error(usage)
      const target = args.slice(1).join(" ")
      let shortcut = target
      try { shortcut = parseShortcut(target).label } catch { /* Action IDs and commands are not shortcut labels. */ }
      const custom = customRows().find((row) => row.label === shortcut || row.detail?.startsWith(`Command: ${target}.`))
      if (custom) { openCustom(context, custom); return }
      const setting = findBindingSetting(target)
      if (!setting) throw new Error(`Unknown keybinding action: ${target}. Use /keybind to browse current actions.`)
      openDetails(context, setting)
      return
    }
    if (args.length && args[0] !== "list") throw new Error(usage)
    if (args.length > 2 || (args[1] && !bindingScopes.some((scope) => scope === args[1]))) throw new Error(usage)
    const scope = args[1]
    context.actions.openPicker({
      title: `Keybindings${scope ? ` · ${scope}` : ""}`,
      rows: [...customRows(), ...bindingSettings().map(settingRow)].filter((row) => !scope || row.tag === scope),
      detailPane: true,
      maxRows: 8,
      hint,
      onSelect: (row) => {
        if (row.id.startsWith("custom:")) { openCustom(context, row); return }
        const setting = findBindingSetting(row.id)
        if (setting) openDetails(context, setting)
      },
    })
  },
}
