import { afterEach, expect, test } from "bun:test"
import { mkdtempSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { HyaClient, SkillListing } from "../src/client"
import { createReloader, reloadAllTargets, reloadSummary } from "../src/app/reload"
import { createCommandRegistry, type AppActions } from "../src/commands"
import { customKeybindings, setCustomKeybindings } from "../src/keys/custom"
import { createAppStore } from "../src/state/store"
import { autoThemeSetting, setThemeSetting, themeSetting } from "../src/theme"

afterEach(() => {
  setCustomKeybindings({})
  setThemeSetting(autoThemeSetting)
})

function prefs(content: object): string {
  const dir = mkdtempSync(join(tmpdir(), "hya-reload-"))
  const path = join(dir, "tui.json")
  writeFileSync(path, JSON.stringify(content))
  return path
}

function fakeClient(calls: string[], overrides: Partial<HyaClient> = {}): HyaClient {
  return {
    refreshBundles: async () => { calls.push("refreshBundles") },
    getMcpStatus: async () => { calls.push("mcp"); return [{ name: "echo", state: "MCP_SERVER_STATE_CONNECTED" }, { name: "broken", state: "MCP_SERVER_STATE_FAILED" }] },
    listCommands: async () => { calls.push("commands"); return [{ name: "review", source: "command" }, { name: "skill:greet", source: "skill" }] },
    refreshSessionContext: async (id: string) => { calls.push(`context ${id}`); return { pending: true } },
    ...overrides,
  } as unknown as HyaClient
}

const listing: SkillListing = { skills: [{ name: "greet" }, { name: "review" }], diagnostics: [{ message: "x" }] }

test("/reload runs every target in order once, refreshing bundles a single time, and says what each did", async () => {
  const calls: string[] = []
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  const reloader = createReloader({
    store,
    client: fakeClient(calls),
    preferencesPath: prefs({ keybindings: { F5: { command: "/status", scope: "workspace" } }, theme: "light" }),
    refreshExtensions: async () => { calls.push("extensions") },
    readSkills: async () => { calls.push("skills"); store.setSkills(listing); return listing },
  })
  const outcomes = await reloader.run(reloadAllTargets)
  expect(calls).toEqual(["refreshBundles", "extensions", "mcp", "commands", "skills", "context hysec_1"])
  expect(outcomes.map((outcome) => outcome.target)).toEqual([...reloadAllTargets])
  expect(Object.keys(customKeybindings())).toEqual(["F5"])
  expect(themeSetting()).toBe("light")
  expect(store.state.backendCommands.map((command) => command.name)).toEqual(["review", "skill:greet"])
  expect(reloadSummary(outcomes)).toBe([
    "Reloaded keybindings (1 custom), theme light, bundles, 0 TUI extensions, 1 command, 2 skills (1 warning), context (re-recorded at the next turn)",
    "Failed: tui.toml: not used by this client",
    "Failed: MCP (1 connected, 1 failed: broken; /mcp to fix)",
  ].join("\n"))
})

test("/reload-skills alone re-records the session's skill index; /reload-mcp refreshes the declarations first", async () => {
  const calls: string[] = []
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  const reloader = createReloader({ store, client: fakeClient(calls), readSkills: async () => { calls.push("skills"); return listing } })
  const [skills] = await reloader.run(["skills"])
  expect(skills).toEqual({ target: "skills", ok: true, text: "2 skills (1 warning), context (re-recorded at the next turn)" })
  await reloader.run(["mcp"])
  expect(calls).toEqual(["skills", "context hysec_1", "refreshBundles", "mcp"])
})

test("a failing target is reported and the next one still runs", async () => {
  const calls: string[] = []
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  const client = fakeClient(calls, {
    refreshBundles: async () => { throw new Error("hya/x: boom") },
    refreshSessionContext: async () => { throw new Error("session_busy: session is busy") },
  })
  const reloader = createReloader({ store, client, readSkills: async () => listing })
  const outcomes = await reloader.run(["bundles", "commands", "context"])
  expect(outcomes).toEqual([
    { target: "bundles", ok: false, text: "bundles: hya/x: boom" },
    { target: "commands", ok: true, text: "1 command" },
    { target: "context", ok: false, text: "context: a turn is running; /reload-context after it finishes" },
  ])
})

test("context without a session, an unknown theme, and the TUI without a launcher", async () => {
  const reloader = createReloader({ store: createAppStore(), client: fakeClient([]), preferencesPath: prefs({ theme: "nope" }), readSkills: async () => listing })
  expect(await reloader.run(["context", "theme", "tui"])).toEqual([
    { target: "context", ok: true, text: "context (no open session)" },
    { target: "theme", ok: false, text: expect.stringContaining("theme: unknown theme nope") },
    { target: "tui", ok: false, text: "TUI: no launcher to restart it; restart hya to load new TUI code" },
  ])
  expect(themeSetting()).toBe(autoThemeSetting)
  let restarted = 0
  const supervised = createReloader({ store: createAppStore(), client: fakeClient([]), readSkills: async () => listing, reloadTui: () => { restarted++ } })
  expect(await supervised.run(["tui"])).toEqual([{ target: "tui", ok: true, text: "TUI (restarting)" }])
  expect(restarted).toBe(1)
})

test("/reload and /reload-<target> put the summary into the transcript; /reload-layout runs /layout reload", async () => {
  const store = createAppStore()
  const ran: string[] = []
  const actions = {
    reload: async (targets) => { ran.push(targets.join(",")); return targets.map((target) => ({ target, ok: true, text: target })) },
    loadPaneLayout: () => { ran.push("layout"); throw new Error("The layout tree belongs to the workspace preset") },
  } as Partial<AppActions> as AppActions
  const registry = createCommandRegistry()
  const context = { store, client: {} as HyaClient, actions }
  await registry.dispatch("/reload", context)
  expect(ran).toEqual([reloadAllTargets.join(",")])
  expect(store.state.dividers.at(-1)?.text.split("\n")[0]).toBe(`Reloaded ${reloadAllTargets.join(", ")}`)
  await registry.dispatch("/reload-commands", context)
  expect(ran.at(-1)).toBe("commands")
  expect(store.state.dividers.at(-1)?.text).toBe("Reloaded commands")
  for (const name of ["/reload-config", "/reload-keybindings", "/reload-theme", "/reload-extensions", "/reload-bundles", "/reload-mcp", "/reload-commands", "/reload-skills", "/reload-context", "/reload-layout", "/reload-tui"]) {
    expect(registry.get(name)?.description).toBeTruthy()
  }
  await expect(registry.dispatch("/reload-layout", context)).rejects.toThrow()
})
