/**
 * `/config` and `/set` (ADR-0036, docs/tui-config.md "Commands"): show,
 * reload, edit, and create `tui.toml`, and change the structure for this
 * session only. Nothing here writes `tui.toml` except `/config init` (and
 * `/config edit` on a missing file), which never overwrites one.
 */
import { switchableComponents } from "../config/presets"
import { startupInput } from "../app/startupInput"
import { startupLines } from "../state/startup"
import type { AppActions, CommandContext, CommandSpec } from "./registry"
import { matchValues } from "./registry"
import { parseSwitch } from "../state/layout"
import type { StructureControl, SwitchableComponent } from "../app/structure"

const configUsage = "Usage: /config [reload|edit|init]"
const setUsage = `Usage: /set preset <name> | /set <${switchableComponents.join("|")}> on|off`

function control(actions: AppActions): StructureControl {
  if (!actions.structure) throw new Error("TUI config unavailable in this client")
  return actions.structure
}

/** Show the `/config` view: path, preset, every component value with its source, and the warnings. */
export function showConfig({ store, actions }: CommandContext): void {
  store.setStatusText(control(actions).describe())
  store.setView("config")
}

/** `/sidebar on` and friends in a single-column preset: `/set sidebar on` for this session. */
export function enableSidebar(actions: AppActions): void {
  control(actions).setEnabled("sidebar", true)
}

/** Why a `/layout` edit is refused outside the workspace structure. */
export const layoutNeedsWorkspace = "The layout tree belongs to the workspace preset · /set preset workspace for this session, or preset = \"workspace\" in tui.toml"

export const configCommand: CommandSpec = {
  name: "/config",
  description: "Show the TUI structure (tui.toml): preset, components, sources, warnings; reload, edit, or init it",
  argumentHint: "[reload|edit|init]",
  complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, ["reload", "edit", "init"]) : [],
  run: async (context, { args }) => {
    const structure = control(context.actions)
    if (args.length > 1) throw new Error(configUsage)
    switch (args[0]) {
      case undefined:
        break
      case "reload": {
        const resolved = structure.reload()
        context.store.setStatus(`Reloaded tui.toml · preset ${resolved.preset}`)
        break
      }
      case "init":
        structure.init()
        context.store.setStatus(`Wrote ${structure.path}`)
        break
      case "edit":
        context.store.setStatus(await structure.edit())
        break
      default:
        throw new Error(configUsage)
    }
    showConfig(context)
  },
}

export const setCommand: CommandSpec = {
  name: "/set",
  description: "Change the TUI structure for this session (not saved): a preset, or a component on or off",
  argumentHint: "preset <name> | <component> on|off",
  complete: ({ words, current, head }, completion) => {
    if (words.length === 1) return matchValues(head, current, ["preset", ...switchableComponents])
    if (words.length === 2 && words[0] === "preset") return matchValues(head, current, completion.presets ?? [])
    if (words.length === 2 && (switchableComponents as readonly string[]).includes(words[0]!)) return matchValues(head, current, ["on", "off"])
    return []
  },
  run: (context, { args }) => {
    const structure = control(context.actions)
    const [target, value] = args
    if (args.length !== 2 || !target || !value) throw new Error(setUsage)
    if (target === "preset") {
      structure.setPreset(value)
      context.store.setStatus(`Preset ${value} for this session · tui.toml unchanged`)
      return
    }
    if (!(switchableComponents as readonly string[]).includes(target)) throw new Error(setUsage)
    const enabled = parseSwitch(value, false, setUsage)
    structure.setEnabled(target as SwitchableComponent, enabled)
    context.store.setStatus(`${target} ${enabled ? "on" : "off"} for this session · tui.toml unchanged`)
  },
}

const pasteUsage = "Usage: /paste expand [n]"

/** `/paste expand [n]` (docs/tui.md "Paste and attachments"): a paste or file chip back to editable text. */
export const pasteCommand: CommandSpec = {
  name: "/paste",
  description: "Expand a paste or file chip of the message input into editable text",
  argumentHint: "expand [n]",
  complete: ({ words, current, head }) => words.length === 1 ? matchValues(head, current, ["expand"]) : [],
  run: ({ store, actions }, { args }) => {
    if (args[0] !== "expand" || args.length > 2 || (args[1] !== undefined && !/^[1-9]\d*$/.test(args[1]))) throw new Error(pasteUsage)
    const index = args[1] === undefined ? undefined : Number(args[1])
    if (!actions.expandChip?.(index)) throw new Error(index === undefined ? "No paste or file chip in the input" : `No paste or file chip ${index} in the input`)
    store.setStatus("Expanded the chip into text")
  },
}

/** `/resources`: the full startup list again, at the end of the transcript (after `/reload`, too). */
export const resourcesCommand: CommandSpec = {
  name: "/resources",
  description: "List the loaded context files, skills, commands, MCP servers, bundles, and TUI extensions, grouped by source",
  run: ({ store }) => {
    const settings = store.state.structure.startup
    const lines = startupLines({ ...startupInput(store.state), notices: [] }, { resources: "full", quiet: false, sections: settings.sections }, Math.max(20, store.state.columns - 4))
    store.setView("chat")
    store.addTranscriptNotice(lines.length ? lines.map((line) => line.text).join("\n") : "Nothing loaded yet")
  },
}
