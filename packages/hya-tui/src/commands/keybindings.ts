/** Stable command entry point for keybinding inspection and future settings. */
import { bindingScopes, bindingSettings, findBindingSetting, type BindingSetting } from "../keys/catalog"
import type { PickerRow } from "../state/picker"
import { matchValues, type CommandContext, type CommandSpec } from "./registry"

const usage = "Usage: /keybindings [list [workspace|conversation|pane] | show <action or command>]"
const hint = "↑↓ select · type to filter · Enter details · Esc closes"

function settingRow(setting: BindingSetting): PickerRow {
  return {
    id: setting.id,
    label: setting.id,
    tag: setting.scope,
    detail: `Keys: ${setting.keys.join(" / ")}. ${setting.command ? `Command: ${setting.command}. ` : ""}${setting.context} ${setting.description}`,
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

export const keybindingsCommand: CommandSpec = {
  name: "/keybindings",
  description: "Inspect current keybindings, their related commands and routing scopes",
  argumentHint: "[list [scope] | show <action or command>]",
  complete: ({ words, current, head }) => {
    if (words.length === 1) return matchValues(head, current, ["list", "show"])
    if (words.length === 2 && words[0] === "list") return matchValues(head, current, [...bindingScopes])
    if (words.length === 2 && words[0] === "show") {
      return matchValues(head, current, bindingSettings().flatMap((setting) => [setting.id, ...(setting.command ? [setting.command] : [])]))
    }
    return []
  },
  run: (context, { args }) => {
    if (args[0] === "show") {
      if (args.length < 2) throw new Error(usage)
      const target = args.slice(1).join(" ")
      const setting = findBindingSetting(target)
      if (!setting) throw new Error(`Unknown keybinding action: ${target}. Use /keybindings to browse current actions.`)
      openDetails(context, setting)
      return
    }
    if (args.length && args[0] !== "list") throw new Error(usage)
    if (args.length > 2 || (args[1] && !bindingScopes.some((scope) => scope === args[1]))) throw new Error(usage)
    const scope = args[1]
    context.actions.openPicker({
      title: `Keybindings${scope ? ` · ${scope}` : ""}`,
      rows: bindingSettings().filter((setting) => !scope || setting.scope === scope).map(settingRow),
      detailPane: true,
      maxRows: 8,
      hint,
      onSelect: (row) => {
        const setting = findBindingSetting(row.id)
        if (setting) openDetails(context, setting)
      },
    })
  },
}
