/**
 * `/reload` and `/reload-<target>` (docs/tui.md "Reload"): read a resource
 * again without restarting. Three kinds of target:
 *
 * - this TUI's own files: `config` (`tui.toml`), `keybindings` and `theme`
 *   (`tui.json`), `extensions` (the bundle TUI extensions), and `tui` (its
 *   code, through the launcher's supervisor, src/reload.ts);
 * - the backend's hot reload (ADR-0031 rule 1): `bundles` (bundles,
 *   plugins, MCP declarations, the Project tier: `RefreshBundles`), `mcp`
 *   (its servers' state after that), `commands` and `skills` (the catalogs
 *   the command menu, the startup list, and `/skills` read);
 * - the open session's recorded context: `context` (`RefreshSessionContext`:
 *   the environment, `AGENTS.md`, and skill index its next turn records).
 *
 * A failing target does not stop the next one; each says what it did. A
 * running turn keeps its binding: what changed applies from the next turn.
 */
import type { CommandSummary, HyaClient, SkillListing } from "../client"
import { customKeybindings, setCustomKeybindings } from "../keys/custom"
import { extensionManager } from "../extensions/manager"
import { loadPreferences } from "../prefs"
import { serverStateText } from "../state/mcp"
import { errorText } from "../state/providers"
import type { AppStore } from "../state/store"
import { autoThemeSetting, resolveThemeSetting, setThemeSetting, themeSetting } from "../theme"
import type { StructureControl } from "./structure"
import { type ReloadOutcome, type ReloadTarget } from "../state/reload"

export { reloadAllTargets, reloadSummary, reloadTargetDescriptions, reloadTargets, type ReloadOutcome, type ReloadTarget } from "../state/reload"

export interface ReloaderOptions {
  store: AppStore
  client: HyaClient
  structure?: StructureControl
  /** `tui.json`; without one keybindings and theme have nothing to read. */
  preferencesPath?: string
  /** Re-read the scope's TUI extension catalog and load it (app/run.tsx). */
  refreshExtensions?: () => Promise<void>
  /** `ListSkills` and `ListCommands` into the store (app/controller.ts `readSkills`). */
  readSkills: () => Promise<SkillListing>
  /** `ListCommands` with its diagnostics into the store (app/controller.ts `readCommands`); else a plain `listCommands`. */
  readCommands?: () => Promise<CommandSummary[]>
  /** Restart the TUI process through its supervisor; absent without one. */
  reloadTui?: () => void
}

const plural = (count: number, word: string): string => `${count} ${word}${count === 1 ? "" : "s"}`

export function createReloader({ store, client, structure, preferencesPath, refreshExtensions, readSkills, readCommands, reloadTui }: ReloaderOptions) {
  const ok = (target: ReloadTarget, text: string): ReloadOutcome => ({ target, ok: true, text })
  const failed = (target: ReloadTarget, text: string): ReloadOutcome => ({ target, ok: false, text })

  async function one(target: ReloadTarget, plan: ReadonlySet<ReloadTarget>, done: ReadonlySet<ReloadTarget>): Promise<ReloadOutcome> {
    switch (target) {
      case "config": {
        if (!structure) return failed(target, "tui.toml: not used by this client")
        const resolved = structure.reload()
        return ok(target, `tui.toml (preset ${resolved.preset}${resolved.warnings.length ? `, ${plural(resolved.warnings.length, "warning")}` : ""})`)
      }
      case "keybindings": {
        if (!preferencesPath) return failed(target, "keybindings: no tui.json")
        const loaded = loadPreferences(preferencesPath)
        setCustomKeybindings(loaded.preferences.keybindings ?? {})
        const count = Object.keys(customKeybindings()).length
        return loaded.warning ? failed(target, `keybindings: ${loaded.warning}`) : ok(target, `keybindings (${count} custom)`)
      }
      case "theme": {
        if (!preferencesPath) return failed(target, "theme: no tui.json")
        const loaded = loadPreferences(preferencesPath)
        const wanted = loaded.preferences.theme ?? autoThemeSetting
        if (!setThemeSetting(wanted)) {
          setThemeSetting(autoThemeSetting)
          return failed(target, `theme: unknown theme ${wanted} in ${preferencesPath}; using auto`)
        }
        const setting = themeSetting()
        return ok(target, `theme ${setting}${setting === autoThemeSetting ? ` (${resolveThemeSetting(setting)})` : ""}`)
      }
      case "extensions": {
        await refreshExtensions?.()
        const extensions = extensionManager.list()
        for (const extension of extensions) {
          if (extension.state === "running" || extension.state === "failed") await extensionManager.reload(extension.id)
        }
        const broken = extensionManager.list().filter((extension) => extension.state === "failed")
        if (broken.length) return failed(target, `TUI extensions: ${broken.map((extension) => `${extension.id} failed`).join(", ")}`)
        return ok(target, plural(extensions.length, "TUI extension"))
      }
      case "bundles": {
        await client.refreshBundles()
        // The extension catalog follows the bundles, unless `extensions` reloads it next.
        if (!plan.has("extensions")) await refreshExtensions?.()
        return ok(target, "bundles")
      }
      case "mcp": {
        // `RefreshBundles` reads the MCP declarations; the servers' state follows.
        if (!done.has("bundles")) await client.refreshBundles()
        const servers = await client.getMcpStatus()
        store.setMcpServers(servers)
        if (!servers.length) return ok(target, "MCP (no servers)")
        const counts = new Map<string, number>()
        for (const server of servers) {
          const state = serverStateText(server.state)
          counts.set(state, (counts.get(state) ?? 0) + 1)
        }
        const summary = [...counts].map(([state, count]) => `${count} ${state}`).join(", ")
        const failedNames = servers.filter((server) => serverStateText(server.state) === "failed").map((server) => server.name)
        return failedNames.length ? failed(target, `MCP (${summary}: ${failedNames.join(", ")}; /mcp to fix)`) : ok(target, `MCP (${summary})`)
      }
      case "commands": {
        const commands = readCommands ? await readCommands() : await client.listCommands()
        if (!readCommands) store.setBackendCommands(commands)
        return ok(target, plural(commands.filter((command) => command.source !== "skill").length, "command"))
      }
      case "skills": {
        const listing = await readSkills()
        const skills = ok(target, `${plural(listing.skills.length, "skill")}${listing.diagnostics.length ? ` (${plural(listing.diagnostics.length, "warning")})` : ""}`)
        // The open session's skill index is part of its recorded context.
        if (plan.has("context")) return skills
        const context = await one("context", plan, done)
        return { ...skills, ok: context.ok, text: `${skills.text}, ${context.text}` }
      }
      case "context": {
        const session = store.state.selected
        if (!session) return ok(target, "context (no open session)")
        try {
          const { pending } = await client.refreshSessionContext(session.id)
          return ok(target, pending ? "context (re-recorded at the next turn)" : "context (read at the first turn)")
        } catch (error) {
          const text = errorText(error)
          return failed(target, /busy/i.test(text) ? "context: a turn is running; /reload-context after it finishes" : `context: ${text}`)
        }
      }
      case "layout":
        return failed(target, "layout: run /layout reload")
      case "tui": {
        if (!reloadTui) return failed(target, "TUI: no launcher to restart it; restart hya to load new TUI code")
        reloadTui()
        return ok(target, "TUI (restarting)")
      }
    }
  }

  /** Reload `targets` in order; every target runs and reports, whatever the others did. */
  async function run(targets: readonly ReloadTarget[]): Promise<ReloadOutcome[]> {
    const plan = new Set(targets)
    const done = new Set<ReloadTarget>()
    const outcomes: ReloadOutcome[] = []
    for (const target of targets) {
      try {
        outcomes.push(await one(target, plan, done))
      } catch (error) {
        outcomes.push(failed(target, `${target}: ${errorText(error)}`))
      }
      done.add(target)
    }
    return outcomes
  }

  return { run }
}
