/**
 * TUI structure presets (ADR-0036, docs/tui-config.md): the built-in
 * component settings that `tui.toml` selects with `preset` and overrides key
 * by key. A preset is a full set of component values, so every lookup has a
 * value; `tui.toml` layers only change what they name.
 */

/** Status line segments in their default order (docs/tui-config.md "Status line segments"). */
export const statusSegments = ["model", "mode", "ctx", "connection", "agent", "cwd", "git", "tokens", "session"] as const
export type StatusSegment = typeof statusSegments[number]

/** What the status line shows by default, left to right. */
export const defaultSegments: readonly StatusSegment[] = ["model", "mode", "cwd", "git", "ctx", "tokens"]

/** `[composer.paste]`: when a text paste folds into a chip, and how chips are sent (composer/chips.ts). */
export interface PasteSettings {
  /** More lines than this fold into a chip; 0 never folds. */
  collapse_lines: number
  /** Or more characters than this. */
  collapse_chars: number
  /** Send pastes and files as `<attachment>` blocks (true) or as raw text. */
  wrap: boolean
  /** Convert an image type the server refuses to PNG, and scale one over 10 MiB down (composer/images.ts). */
  image_resize: boolean
}

/** Sections of the startup list (state/startup.ts), in their default order. */
export const startupSections = ["context", "skills", "commands", "mcp", "bundles", "extensions"] as const
export type StartupSection = typeof startupSections[number]

/** `[startup]`: the list at the start of the transcript. */
export interface StartupSettings {
  enabled: boolean
  /** `compact`: one line per section; `full`: grouped by source with paths. */
  resources: "compact" | "full"
  sections: StartupSection[]
  /** Only problem items. */
  quiet: boolean
}

/** The resolved structure the TUI draws. Field names match `tui.toml`. */
export interface TuiStructure {
  startup: StartupSettings
  /** Open todo rows above the input (the dock). */
  todos: { enabled: boolean; max_rows: number }
  /** Running subagent rows above the input (the dock). */
  subagents: { enabled: boolean }
  statusline: { enabled: boolean; segments: StatusSegment[]; placement: "below" | "band" }
  composer: { style: "box" | "band"; border_info: StatusSegment[]; paste: PasteSettings }
  sidebar: { enabled: boolean }
}
export type ComponentName = keyof TuiStructure

/** Components whose `enabled` switch `/set <component> on|off` changes for the session. */
export const switchableComponents = ["startup", "statusline", "todos", "subagents", "sidebar"] as const satisfies readonly ComponentName[]

export const builtinPresetNames = ["minimal", "statusline", "dock", "workspace"] as const
export type BuiltinPreset = typeof builtinPresetNames[number]
export const defaultPreset: BuiltinPreset = "statusline"

/** Built-in presets this version draws. */
export const availablePresets: readonly BuiltinPreset[] = ["minimal", "statusline", "dock", "workspace"]

const base = (): TuiStructure => ({
  startup: { enabled: true, resources: "compact", sections: [...startupSections], quiet: false },
  todos: { enabled: false, max_rows: 3 },
  subagents: { enabled: false },
  statusline: { enabled: true, segments: [...defaultSegments], placement: "below" },
  composer: { style: "box", border_info: [], paste: { collapse_lines: 10, collapse_chars: 1000, wrap: true, image_resize: true } },
  sidebar: { enabled: false },
})

/** The full component values of a built-in preset. */
export function builtinPreset(name: BuiltinPreset): TuiStructure {
  const structure = base()
  switch (name) {
    case "minimal":
      structure.statusline.enabled = false
      structure.composer.border_info = ["model", "mode"]
      break
    case "statusline":
      break
    case "dock":
      structure.composer.style = "band"
      structure.statusline.placement = "band"
      structure.todos.enabled = true
      structure.subagents.enabled = true
      break
    case "workspace":
      structure.sidebar.enabled = true
      structure.todos.enabled = true
      structure.subagents.enabled = true
      break
  }
  return structure
}

export function isBuiltinPreset(name: string): name is BuiltinPreset {
  return (builtinPresetNames as readonly string[]).includes(name)
}

/** One-line description per built-in preset (`/config`, `/set preset` completion, the template). */
export const presetDescriptions: Record<BuiltinPreset, string> = {
  minimal: "transcript and input; model and mode on the input border",
  statusline: "transcript, input, and one status row below it (default)",
  dock: "status band right above a ╰─ input, with open todo and running subagent rows",
  workspace: "the tiled workspace: Projects, Sessions, Todos, Context sidebars and the layout tree",
}
