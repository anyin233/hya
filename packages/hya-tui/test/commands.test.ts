import { expect, test } from "bun:test"
import type { HyaClient } from "../src/client"
import { nativeCommands } from "../src/completion"
import { createCommandRegistry, type AppActions } from "../src/commands"
import { createAppStore } from "../src/state/store"

function harness(client: Partial<HyaClient> = {}) {
  const store = createAppStore()
  const calls: string[] = []
  const actions: AppActions = {
    refresh: async () => { calls.push("refresh") },
    refreshMessages: async () => { calls.push("refreshMessages") },
    openSession: async (id) => { calls.push(`open ${id}`) },
    newSession: async (agent, model) => { calls.push(`new ${agent ?? ""} ${model ?? ""}`.trim()) },
    beginKeyEntry: (provider) => { calls.push(`key ${provider}`) },
    scheduleRefresh: () => { calls.push("scheduleRefresh") },
  }
  const registry = createCommandRegistry()
  const context = { store, client: client as HyaClient, actions }
  return { store, calls, registry, run: (text: string) => registry.dispatch(text, context) }
}

test("registers every native slash command with a description", () => {
  const { registry } = harness()
  expect(registry.names().sort()).toEqual([...nativeCommands].sort())
  for (const name of registry.names()) expect(registry.get(name)?.description.length).toBeGreaterThan(0)
  expect(registry.get("/key")?.argumentHint).toBe("set|remove <provider>")
})

test("parses a command line into name, words, and the raw argument text", () => {
  const { registry } = harness()
  expect(registry.parse("/answer req_1  yes please")).toEqual({
    name: "/answer", args: ["req_1", "yes", "please"], argumentsText: "req_1  yes please", text: "/answer req_1  yes please",
  })
})

test("view commands switch the main panel", async () => {
  const { store, calls, run } = harness()
  await run("/help")
  expect(store.state.view).toBe("help")
  await run("/models")
  expect(store.state.view).toBe("models")
  expect(calls).toEqual(["refresh"])
})

test("/open resolves list numbers and /login starts concealed key entry", async () => {
  const { store, calls, run } = harness()
  store.applyCatalog({ sessions: [{ id: "hysec_a", agent: "build", workdir: "/w" }, { id: "hysec_b", agent: "build", workdir: "/w" }], interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [] })
  await run("/open 2")
  await run("/open hysec_x")
  await run("/login openai")
  await run("/key set anthropic")
  expect(calls).toEqual(["open hysec_b", "open hysec_x", "key openai", "key anthropic"])
})

test("usage errors are thrown for incomplete commands", async () => {
  const { run } = harness()
  await expect(run("/open")).rejects.toThrow("Usage: /open <session id or number>")
  await expect(run("/key")).rejects.toThrow("Usage: /key set|remove <provider> or /login <provider>")
  await expect(run("/cancel")).rejects.toThrow("No active turn")
  await expect(run("/answer req_1")).rejects.toThrow("Usage: /answer <interaction id> <text>")
})

test("unknown slash commands become backend command turns", async () => {
  const sent: string[] = []
  const { store, calls, run } = harness({
    createCommandTurn: async (session: string, command: string, argumentsText: string) => {
      sent.push(`${session} ${command} ${argumentsText}`)
      return { id: "msg_c", state: "TURN_STATE_RUNNING" }
    },
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/compact  now please")
  expect(sent).toEqual(["hysec_1 compact now please"])
  expect(store.state.turnId).toBe("msg_c")
  expect(store.state.status).toBe("Command compact · turn_state_running")
  expect(calls).toEqual(["scheduleRefresh"])
})

test("argument completion comes from the command's own completer", () => {
  const { registry, store } = harness()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [{ name: "release" }], providers: [{ id: "openai" }], savedKeys: [], commands: [] })
  expect(registry.complete("/workflow run r", store.completionContext())).toEqual(["/workflow run release"])
  expect(registry.complete("/login o", store.completionContext())).toEqual(["/login openai"])
  expect(registry.complete("/unknown x", store.completionContext())).toEqual([])
})
