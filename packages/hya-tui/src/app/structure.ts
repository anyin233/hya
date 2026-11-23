/**
 * The component structure in effect (ADR-0036, docs/tui-config.md): reads
 * `tui.toml` (config/tuiConfig.ts), keeps the session layer of `/set`, and
 * applies the result to the store — the structure, the start-of-transcript
 * notices, and the pane tree (D10): the saved `paneLayout` while the
 * sidebar component is on (the `workspace` preset), the fixed chat column
 * otherwise. The chat column is never saved.
 */
import { batch } from "solid-js"
import { homedir } from "node:os"
import { builtinPresetNames, defaultPreset, presetDescriptions, statusSegments, switchableComponents, type ComponentName, type TuiStructure } from "../config/presets"
import {
  initTuiConfig, loadTuiConfig, presetNames, resolveTuiConfig,
  type LoadedTuiConfig, type ResolvedTuiConfig, type SessionOverrides,
} from "../config/tuiConfig"
import { loadPreferences, savePreferences } from "../prefs"
import { chatPaneLayout, defaultPaneLayout, type PaneLayout } from "../state/panes"
import type { AppStore } from "../state/store"

export type SwitchableComponent = typeof switchableComponents[number]

export interface StructureControl {
  /** The `tui.toml` this TUI reads. */
  readonly path: string
  /** The structure in effect, with its sources and warnings. */
  resolved(): ResolvedTuiConfig
  /** Read `tui.toml` again and apply it; the session layer stays. */
  reload(): ResolvedTuiConfig
  /** `/set preset <name>`: throws for an unknown name. */
  setPreset(name: string): void
  /** `/set <component> on|off`. */
  setEnabled(component: SwitchableComponent, enabled: boolean): void
  /** Write the template; throws when the file exists. */
  init(): void
  /** Open `tui.toml` in the external editor (writing the template first when it is missing), then reload. */
  edit(): Promise<string>
  /** Names `/set preset` accepts. */
  presets(): string[]
  /** The `/config` view text. */
  describe(): string
}

export interface StructureOptions {
  store: AppStore
  /** `tui.toml` (config/tuiConfig.ts `tuiConfigPath`). */
  path: string
  /** `tui.json`: the saved `paneLayout` and the one-time notice record. */
  preferencesPath?: string
  /** Run the external editor on a file (composer/editor.ts `editFile`). */
  editFile?(path: string): Promise<{ ok: true } | { ok: false; error: string }>
}

/** Id of the one-time notice that the default changed (recorded in `tui.json` `seenNotices`). */
export const statuslineDefaultNotice = "statusline-default"
export const statuslineDefaultText = `[Layout] The default layout is now ${defaultPreset}. For the sidebars, write preset = "workspace" in tui.toml (/config init writes a template), or run /set preset workspace for this session.`

const shortPath = (path: string): string => {
  const home = homedir()
  return home && path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path
}

const tomlValue = (value: unknown): string => Array.isArray(value) ? `[${value.map((item) => JSON.stringify(item)).join(", ")}]` : typeof value === "string" ? JSON.stringify(value) : String(value)

export function createStructureController({ store, path, preferencesPath, editFile }: StructureOptions): StructureControl {
  const session: SessionOverrides = {}
  let loaded: LoadedTuiConfig = { path, exists: false, warnings: [] }
  let current = resolveTuiConfig(undefined)
  let layoutActive: boolean | undefined
  let upgradeNotice = false

  const savedLayout = (): PaneLayout => (preferencesPath ? loadPreferences(preferencesPath).preferences.paneLayout : undefined) ?? defaultPaneLayout()

  const warnings = (): string[] => [...loaded.warnings, ...current.warnings]

  function notices(): string[] {
    const lines: string[] = []
    if (upgradeNotice) lines.push(statuslineDefaultText)
    const all = warnings()
    if (all.length) lines.push(`[Config] ${shortPath(path)} · ${all.length} warning${all.length === 1 ? "" : "s"} · /config lists them`, ...all.map((warning) => `  ${warning}`))
    return lines
  }

  function apply(): void {
    current = resolveTuiConfig(loaded.raw, session)
    const workspace = current.structure.sidebar.enabled
    batch(() => {
      store.setStructure(current.structure)
      if (workspace !== layoutActive) {
        layoutActive = workspace
        store.setPaneLayout(workspace ? savedLayout() : chatPaneLayout())
      }
      store.setStartupNotices(notices())
    })
  }

  function read(): void {
    const first = layoutActive === undefined
    loaded = loadTuiConfig(path)
    // Once per user: a saved tiled layout and no tui.toml means the sidebars just went away.
    if (first && !loaded.exists && preferencesPath) {
      const preferences = loadPreferences(preferencesPath).preferences
      const seen = preferences.seenNotices ?? []
      if (preferences.paneLayout && !seen.includes(statuslineDefaultNotice)) {
        upgradeNotice = true
        try { savePreferences(preferencesPath, { seenNotices: [...seen, statuslineDefaultNotice] }) } catch { /* shown again next time */ }
      }
    }
    apply()
  }

  read()

  return {
    path,
    resolved: () => current,
    reload() {
      read()
      return current
    },
    setPreset(name) {
      if (!presetNames(loaded.raw).includes(name)) throw new Error(`Unknown preset ${name} · presets: ${presetNames(loaded.raw).join(", ")}`)
      session.preset = name
      // A preset switch starts that preset's own component values.
      session.enabled = {}
      apply()
    },
    setEnabled(component, enabled) {
      session.enabled = { ...session.enabled, [component]: enabled }
      apply()
    },
    init() {
      initTuiConfig(path)
      read()
    },
    async edit() {
      if (!editFile) throw new Error("External editor unavailable: no terminal")
      if (!loadTuiConfig(path).exists) initTuiConfig(path)
      const result = await editFile(path)
      if (!result.ok) throw new Error(`${result.error} · ${shortPath(path)} unchanged by the TUI`)
      read()
      const count = warnings().length
      return `Reloaded ${shortPath(path)} · preset ${current.preset}${count ? ` · ${count} warning${count === 1 ? "" : "s"}` : ""}`
    },
    presets: () => presetNames(loaded.raw),
    describe() {
      const structure: TuiStructure = current.structure
      const rows: string[] = [
        `File      ${shortPath(path)}${loaded.exists ? "" : " (not found: built-in defaults; /config init writes a template)"}`,
        `Preset    ${current.preset} (${current.presetSource})${current.chain.length > 1 ? ` · ${current.chain.join(" → ")}` : ""}`,
        `Presets   ${presetNames(loaded.raw).join(", ")}`,
        "",
      ]
      const width = Math.max(...(Object.keys(structure) as ComponentName[]).flatMap((component) => Object.keys(structure[component]).map((key) => `${component}.${key}`.length)))
      for (const component of Object.keys(structure) as ComponentName[]) {
        for (const [key, value] of Object.entries(structure[component])) {
          const name = `${component}.${key}`
          rows.push(`${name.padEnd(width + 2)}${tomlValue(value).padEnd(40)} ${current.sources[name] ?? ""}`)
        }
      }
      const all = warnings()
      rows.push("", all.length ? `Warnings (${all.length})` : "Warnings  none", ...all.map((warning) => `  ${warning}`))
      rows.push(
        "",
        `Segments  ${statusSegments.join(", ")}`,
        ...builtinPresetNames.map((name) => `  ${name.padEnd(11)}${presetDescriptions[name]}`),
        "",
        `Usage     /config reload · /config edit · /config init · /set preset <name> · /set <${switchableComponents.join("|")}> on|off`,
      )
      return rows.join("\n")
    },
  }
}
