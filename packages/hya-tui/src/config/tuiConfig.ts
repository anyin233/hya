/**
 * `tui.toml` (ADR-0036, docs/tui-config.md): the declarative structure file.
 * The TUI only reads it; `/config init` is the one writer, and only when the
 * file does not exist.
 *
 * Location, first that applies: `$HYA_TUI_TOML`; `tui.toml` next to
 * `$HYA_TUI_CONFIG` (tests and profiles that move `tui.json` move this file
 * with it); `$XDG_CONFIG_HOME/hya/tui.toml`; `~/.config/hya/tui.toml`.
 *
 * Precedence, later wins: built-in preset, the named preset chain
 * (`[presets.<name>]`, `extends`), the top-level component tables, then the
 * session's `/set`. Tables merge key by key; arrays replace.
 *
 * Errors never block startup (D6): a missing file is silent; a syntax error
 * ignores the file (with its line); an unknown key, a wrong type, or an
 * unknown name ignores that item; an unknown preset or a bad `extends` chain
 * falls back to the default preset; a key of a later version is ignored with
 * "not supported in this version". Every warning is a string for `/config`
 * and the start of the transcript.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { dirname, join } from "node:path"
import {
  availablePresets, builtinPreset, builtinPresetNames, defaultPreset, isBuiltinPreset, presetDescriptions,
  startupSections, statusSegments, switchableComponents, type BuiltinPreset, type ComponentName, type TuiStructure,
} from "./presets"

/** Points the TUI at another `tui.toml`. */
export const tuiTomlEnv = "HYA_TUI_TOML"
/** The schema version this TUI writes and understands. */
export const tuiConfigVersion = 1
/** At most this many named presets in one `extends` chain. */
export const maxPresetDepth = 8

/** `$HYA_TUI_TOML`, else next to `$HYA_TUI_CONFIG`, else `$XDG_CONFIG_HOME/hya/tui.toml`, else `~/.config/hya/tui.toml`. */
export function tuiConfigPath(env: Record<string, string | undefined>): string {
  if (env[tuiTomlEnv]) return env[tuiTomlEnv]!
  if (env.HYA_TUI_CONFIG) return join(dirname(env.HYA_TUI_CONFIG), "tui.toml")
  const base = env.XDG_CONFIG_HOME || join(env.HOME ?? ".", ".config")
  return join(base, "hya", "tui.toml")
}

export interface LoadedTuiConfig {
  path: string
  exists: boolean
  /** The parsed table; absent when the file is missing or unusable. */
  raw?: Record<string, unknown>
  /** File-level problems (unreadable, syntax error). */
  warnings: string[]
}

const isTable = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value)
const parseMessage = (error: unknown): string => (error instanceof Error ? error.message : String(error)).replace(/^TOML Parse error:\s*/, "")

/**
 * The 1-based line of a TOML syntax error: the shortest prefix of `source`
 * that fails with the same message (Bun's parser reports no usable
 * position). `undefined` when no prefix reproduces it.
 */
export function tomlErrorLine(source: string, message: string): number | undefined {
  const lines = source.split("\n")
  for (let count = 1; count <= Math.min(lines.length, 2000); count++) {
    try {
      Bun.TOML.parse(`${lines.slice(0, count).join("\n")}\n`)
    } catch (error) {
      if (parseMessage(error) === message) return count
    }
  }
  return undefined
}

/** Parse `tui.toml` text; never throws. */
export function parseTuiConfig(source: string, path: string): Pick<LoadedTuiConfig, "raw" | "warnings"> {
  try {
    const raw: unknown = Bun.TOML.parse(source)
    return isTable(raw) ? { raw, warnings: [] } : { warnings: [`Ignored ${path}: not a TOML table`] }
  } catch (error) {
    const message = parseMessage(error)
    const line = tomlErrorLine(source, message)
    return { warnings: [`Ignored ${path}${line ? ` (line ${line})` : ""}: ${message}; using the ${defaultPreset} preset`] }
  }
}

/** Read and parse `tui.toml`; never throws. */
export function loadTuiConfig(path: string): LoadedTuiConfig {
  let source: string
  try {
    source = readFileSync(path, "utf8")
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return { path, exists: false, warnings: [] }
    return { path, exists: true, warnings: [`Ignored unreadable ${path}: ${error instanceof Error ? error.message : String(error)}`] }
  }
  return { path, exists: true, ...parseTuiConfig(source, path) }
}

/** Session layer: `/set preset <name>` and `/set <component> on|off`; never written to a file. */
export interface SessionOverrides {
  preset?: string
  enabled?: Partial<Record<typeof switchableComponents[number], boolean>>
}

export interface ResolvedTuiConfig {
  /** The preset in effect (a built-in or a `[presets.*]` name). */
  preset: string
  /** Where `preset` came from. */
  presetSource: "default" | "tui.toml" | "this session"
  /** The `extends` chain from the preset in effect down to its built-in base. */
  chain: string[]
  structure: TuiStructure
  /** `component.key` → the layer that set it (`preset statusline`, `preset focus`, `tui.toml`, `this session`). */
  sources: Record<string, string>
  /** Every problem found while resolving, for `/config` and the transcript. */
  warnings: string[]
}

type Rule = {
  /** The value to store, or `undefined` when the value has the wrong type. */
  read(value: unknown): unknown
  expected: string
  /** A value this version does not draw yet (`placement = "band"` before the dock). */
  later?(value: unknown): boolean
}
/** A nested table (`[composer.paste]`): its keys merge one by one like a component's. */
type TableRule = { table: Record<string, Rule>; laterKeys?: readonly string[] }
const isTableRule = (rule: Rule | TableRule): rule is TableRule => "table" in rule

const bool: Rule = { read: (value) => typeof value === "boolean" ? value : undefined, expected: "true or false" }
const segmentList: Rule = {
  read: (value) => Array.isArray(value) && value.every((item) => typeof item === "string") ? value : undefined,
  expected: `an array of segment names (${statusSegments.join(", ")})`,
}
const integer = (min: number, max = Number.MAX_SAFE_INTEGER): Rule => ({
  read: (value) => Number.isInteger(value) && (value as number) >= min && (value as number) <= max ? value : undefined,
  expected: max === Number.MAX_SAFE_INTEGER ? `an integer ≥ ${min}` : `an integer from ${min} to ${max}`,
})
const oneOf = (values: readonly string[], later: readonly string[] = []): Rule => ({
  read: (value) => typeof value === "string" && values.includes(value) ? value : undefined,
  expected: values.map((value) => JSON.stringify(value)).join(" or "),
  later: (value) => later.includes(value as string),
})

const sectionList: Rule = {
  read: (value) => Array.isArray(value) && value.every((item) => typeof item === "string") ? value : undefined,
  expected: `an array of section names (${startupSections.join(", ")})`,
}
const rules: { [Component in ComponentName]: Record<keyof TuiStructure[Component] & string, Rule | TableRule> } = {
  startup: { enabled: bool, resources: oneOf(["compact", "full"]), sections: sectionList, quiet: bool },
  statusline: { enabled: bool, segments: segmentList, placement: oneOf(["below", "band"], ["band"]) },
  composer: {
    style: oneOf(["box", "band"], ["band"]),
    border_info: segmentList,
    paste: { table: { collapse_lines: integer(0), collapse_chars: integer(1), wrap: bool, image_resize: bool } },
  },
  sidebar: { enabled: bool },
}
/** Keys of a known component that a later version reads. */
const laterKeys: Record<ComponentName, readonly string[]> = {
  startup: [],
  statusline: [],
  composer: ["attachments"],
  sidebar: ["left", "right", "min_columns"],
}
/** Component tables a later version reads (startup list, dock rows, layout tree). */
const laterComponents = ["todos", "subagents", "layout", "transcript"] as const
/** Reserved tables (ADR-0036 D9). */
const reservedTables = ["theme", "keys"] as const
const componentNames = Object.keys(rules) as ComponentName[]

const notSupported = (key: string): string => `${key} is not supported in this version; ignored`

/** Apply one layer's table for `component`; invalid items are skipped with a warning. */
function applyComponent(
  structure: TuiStructure, sources: Record<string, string>, warnings: string[],
  component: ComponentName, table: unknown, layer: string, prefix: string,
): void {
  if (!isTable(table)) {
    warnings.push(`${prefix}${component} must be a table; ignored`)
    return
  }
  applyTable(
    structure[component] as unknown as Record<string, unknown>, rules[component] as Record<string, Rule | TableRule>, laterKeys[component],
    sources, warnings, table, layer, `${prefix}${component}`, component,
  )
}

/** Apply `table` onto `target` key by key; nested tables recurse. */
function applyTable(
  target: Record<string, unknown>, tableRules: Record<string, Rule | TableRule>, later: readonly string[],
  sources: Record<string, string>, warnings: string[], table: Record<string, unknown>, layer: string, prefix: string, path: string,
): void {
  for (const [key, value] of Object.entries(table)) {
    const name = `${prefix}.${key}`
    const rule = tableRules[key]
    if (!rule) {
      warnings.push(later.includes(key) ? notSupported(name) : `Unknown key ${name}; ignored`)
      continue
    }
    if (isTableRule(rule)) {
      if (!isTable(value)) warnings.push(`${name} must be a table; ignored`)
      else applyTable(target[key] as Record<string, unknown>, rule.table, rule.laterKeys ?? [], sources, warnings, value, layer, name, `${path}.${key}`)
      continue
    }
    const read = rule.read(value)
    if (read === undefined) {
      warnings.push(`${name}: expected ${rule.expected}; ignored`)
      continue
    }
    if (rule.later?.(read)) {
      warnings.push(notSupported(`${name} = ${JSON.stringify(read)}`))
      continue
    }
    if (rule === segmentList || rule === sectionList) {
      const known: readonly string[] = rule === segmentList ? statusSegments : startupSections
      const noun = rule === segmentList ? "segment" : "section"
      const names = read as string[]
      const unknown = names.filter((item) => !known.includes(item))
      if (unknown.length) warnings.push(`${name}: unknown ${noun}${unknown.length === 1 ? "" : "s"} ${unknown.join(", ")}; ignored`)
      target[key] = [...new Set(names.filter((item) => !unknown.includes(item)))]
    } else {
      target[key] = read
    }
    sources[`${path}.${key}`] = layer
  }
}

interface PresetLayer { name: string; table: Record<string, unknown> }

/** The user presets of `raw.presets`, minus invalid entries (warned). */
function userPresets(raw: Record<string, unknown>, warnings: string[]): Map<string, Record<string, unknown>> {
  const presets = new Map<string, Record<string, unknown>>()
  if (raw.presets === undefined) return presets
  if (!isTable(raw.presets)) {
    warnings.push("presets must be a table of named presets; ignored")
    return presets
  }
  for (const [name, table] of Object.entries(raw.presets)) {
    if (isBuiltinPreset(name)) warnings.push(`presets.${name}: ${name} is a built-in preset; choose another name. Ignored`)
    else if (!isTable(table)) warnings.push(`presets.${name} must be a table; ignored`)
    else presets.set(name, table)
  }
  return presets
}

/** The built-in base and user layers (outermost ancestor first) of preset `name`, or `undefined` (warned) to fall back. */
function presetChain(name: string, presets: Map<string, Record<string, unknown>>, warnings: string[]): { base: BuiltinPreset; layers: PresetLayer[] } | undefined {
  const layers: PresetLayer[] = []
  let current = name
  for (;;) {
    if (isBuiltinPreset(current)) {
      if (!availablePresets.includes(current)) {
        warnings.push(`${notSupported(`preset ${current}`)}; using ${defaultPreset}`)
        return undefined
      }
      return { base: current, layers }
    }
    const table = presets.get(current)
    if (!table) {
      warnings.push(`Unknown preset ${current}; using ${defaultPreset}`)
      return undefined
    }
    if (layers.some((layer) => layer.name === current)) {
      warnings.push(`presets.${current}: extends cycle (${[...layers.map((layer) => layer.name).reverse(), current].join(" → ")}); using ${defaultPreset}`)
      return undefined
    }
    if (layers.length >= maxPresetDepth) {
      warnings.push(`preset ${name}: more than ${maxPresetDepth} presets in its extends chain; using ${defaultPreset}`)
      return undefined
    }
    layers.unshift({ name: current, table })
    const parent = table.extends
    if (parent !== undefined && typeof parent !== "string") warnings.push(`presets.${current}.extends must be a preset name; extending ${defaultPreset}`)
    current = typeof parent === "string" ? parent : defaultPreset
  }
}

/** Apply a user preset's component tables; other keys are warned. */
function applyPresetLayer(structure: TuiStructure, sources: Record<string, string>, warnings: string[], layer: PresetLayer): void {
  for (const [key, value] of Object.entries(layer.table)) {
    if (key === "extends") continue
    const prefix = `presets.${layer.name}.`
    if ((componentNames as string[]).includes(key)) applyComponent(structure, sources, warnings, key as ComponentName, value, `preset ${layer.name}`, prefix)
    else if ((laterComponents as readonly string[]).includes(key)) warnings.push(notSupported(`${prefix}${key}`))
    else warnings.push(`Unknown key ${prefix}${key}; ignored`)
  }
}

/** `component.key` (and `component.table.key`) → value, for sources and `/config`. */
export function structureEntries(structure: TuiStructure): [string, unknown][] {
  const entries: [string, unknown][] = []
  const walk = (value: Record<string, unknown>, prefix: string) => {
    for (const [key, item] of Object.entries(value)) {
      if (isTable(item)) walk(item, `${prefix}.${key}`)
      else entries.push([`${prefix}.${key}`, item])
    }
  }
  for (const component of componentNames) walk(structure[component] as unknown as Record<string, unknown>, component)
  return entries
}

function seed(name: BuiltinPreset): { structure: TuiStructure; sources: Record<string, string> } {
  const structure = builtinPreset(name)
  const sources = Object.fromEntries(structureEntries(structure).map(([key]) => [key, `preset ${name}`]))
  return { structure, sources }
}

/** The names `preset` and `/set preset` accept: available built-ins and valid `[presets.*]` entries. */
export function presetNames(raw: Record<string, unknown> | undefined): string[] {
  return [...availablePresets, ...(raw ? userPresets(raw, []).keys() : [])]
}

/** Resolve the structure from the parsed file (or none) and the session layer; never throws. */
export function resolveTuiConfig(raw: Record<string, unknown> | undefined, session: SessionOverrides = {}): ResolvedTuiConfig {
  const warnings: string[] = []
  const file = raw ?? {}
  for (const [key, value] of Object.entries(file)) {
    if (key === "version") {
      if (!Number.isInteger(value)) warnings.push("version must be an integer; ignored")
      else if ((value as number) > tuiConfigVersion) warnings.push(`version ${String(value)} is newer than this TUI (${tuiConfigVersion}); reading the keys it knows`)
    } else if (key === "preset") {
      if (typeof value !== "string") warnings.push(`preset must be a preset name; using ${defaultPreset}`)
    } else if (key === "presets" || (componentNames as string[]).includes(key)) {
      // Read below.
    } else if ((laterComponents as readonly string[]).includes(key) || (reservedTables as readonly string[]).includes(key)) {
      warnings.push(notSupported(`[${key}]`))
    } else {
      warnings.push(`Unknown key ${key}; ignored`)
    }
  }
  const presets = userPresets(file, warnings)
  const filePreset = typeof file.preset === "string" ? file.preset : undefined
  let preset = session.preset ?? filePreset ?? defaultPreset
  let presetSource: ResolvedTuiConfig["presetSource"] = session.preset ? "this session" : filePreset ? "tui.toml" : "default"
  let chain = presetChain(preset, presets, warnings)
  if (!chain) {
    preset = defaultPreset
    presetSource = "default"
    chain = { base: defaultPreset, layers: [] }
  }
  const { structure, sources } = seed(chain.base)
  for (const layer of chain.layers) applyPresetLayer(structure, sources, warnings, layer)
  for (const component of componentNames) {
    if (file[component] !== undefined) applyComponent(structure, sources, warnings, component, file[component], "tui.toml", "")
  }
  for (const [component, enabled] of Object.entries(session.enabled ?? {})) {
    if (enabled === undefined) continue
    structure[component as typeof switchableComponents[number]].enabled = enabled
    sources[`${component}.enabled`] = "this session"
  }
  return { preset, presetSource, chain: [...chain.layers.map((layer) => layer.name).reverse(), chain.base], structure, sources, warnings }
}

/** The commented template `/config init` writes: every preset and every v1 field with its default. */
export function tuiConfigTemplate(): string {
  const presets = builtinPresetNames.map((name) => `#   ${name.padEnd(10)} ${presetDescriptions[name]}`).join("\n")
  return `# hya TUI structure (docs/tui-config.md). The TUI only reads this file.
version = ${tuiConfigVersion}

# Built-in presets:
${presets}
# preset = "${defaultPreset}"

# A named preset: start from a built-in one and change single components.
# [presets.focus]
# extends = "minimal"
# statusline = { enabled = true, segments = ["model", "mode", "ctx"] }

# Top-level tables override the preset in effect.
[startup]
# The list of what was loaded, at the start of the transcript; problems always show.
# enabled = true
# resources = "compact"   # compact: one line per section; full: grouped by source
# sections = ["context", "skills", "commands", "mcp", "bundles", "extensions"]
# quiet = false           # true: only problems

[statusline]
# enabled = true
# Segments, left to right: ${statusSegments.join(", ")}
# segments = ["model", "mode", "cwd", "git", "ctx", "tokens"]
# placement = "below"

[composer]
# style = "box"
# Segments written on the input border (the minimal preset uses ["model", "mode"]).
# border_info = []

[composer.paste]
# A text paste longer than this many lines, or characters, becomes a chip; 0 never folds.
# collapse_lines = 10
# collapse_chars = 1000
# Send pastes and files as <attachment> blocks (false: their raw text).
# wrap = true
# Convert BMP/TIFF/HEIC images to PNG and scale images over 10 MiB down.
# image_resize = true

[sidebar]
# true draws the tiled workspace (Projects, Sessions, Todos, Context).
# enabled = false
`
}

/** Write the template when `path` does not exist; throws (with a message) when it does or cannot be written. */
export function initTuiConfig(path: string): void {
  if (existsSync(path)) throw new Error(`${path} already exists; /config edit opens it`)
  mkdirSync(dirname(path), { recursive: true })
  writeFileSync(path, tuiConfigTemplate(), { flag: "wx" })
}
