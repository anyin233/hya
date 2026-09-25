import { expect, test } from "bun:test"
import type { HyaClient } from "../src/client"
import { nativeCommands } from "../src/completion"
import { createCommandRegistry, type AppActions } from "../src/commands"
import { createAppStore } from "../src/state/store"
import type { PickerSpec } from "../src/state/picker"

function harness(client: Partial<HyaClient> = {}) {
  const store = createAppStore()
  const calls: string[] = []
  const pickers: PickerSpec[] = []
  const actions: AppActions = {
    refresh: async () => { calls.push("refresh") },
    refreshMessages: async () => { calls.push("refreshMessages") },
    openSession: async (id) => { calls.push(`open ${id}`) },
    newSession: async (agent, model) => { calls.push(`new ${agent ?? ""} ${model ?? ""}`.trim()) },
    beginKeyEntry: (provider) => { calls.push(`key ${provider}`) },
    scheduleRefresh: () => { calls.push("scheduleRefresh") },
    cancelTurn: async () => { calls.push("cancel") },
    quit: () => { calls.push("quit") },
    openPicker: (picker) => { pickers.push(picker) },
    requestPermissionMode: async (mode) => { calls.push(`mode ${mode}`) },
    openHelp: () => { calls.push("help") },
  }
  const registry = createCommandRegistry()
  const context = { store, client: client as HyaClient, actions }
  return { store, calls, pickers, registry, run: (text: string) => registry.dispatch(text, context) }
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

test("view commands switch the main panel; /help opens the help overlay", async () => {
  const { store, calls, run } = harness()
  await run("/help")
  expect(store.state.view).toBe("chat")
  expect(calls).toEqual(["help"])
  await run("/models")
  expect(store.state.view).toBe("models")
  expect(calls).toEqual(["help", "refresh"])
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
  await run("/deploy  now please")
  expect(sent).toEqual(["hysec_1 deploy now please"])
  expect(store.state.turnId).toBe("msg_c")
  expect(store.state.status).toBe("Command deploy · turn_state_running")
  expect(calls).toEqual(["scheduleRefresh"])
})

test("a backend command turn's user message shows the /name args the user typed", async () => {
  const { store, run } = harness({
    createCommandTurn: async () => ({ id: "msg_u1", state: "TURN_STATE_RUNNING" }),
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/review  src/main.ts")
  expect(store.state.commandDisplay.get("msg_u1")).toBe("/review  src/main.ts")
})

test("/model and /agent with no argument open a picker of the catalog, the session's current value marked", async () => {
  const { store, pickers, run } = harness()
  store.applyCatalog({
    sessions: [], interactions: [], models: [{ id: "openai/gpt", providerId: "openai", modelId: "gpt" }],
    agents: [{ name: "build", description: "Default agent" }], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", model: { providerId: "openai", modelId: "gpt" } })
  await run("/model")
  expect(pickers.at(-1)?.title).toBe("Model")
  expect(pickers.at(-1)?.rows).toEqual([{ id: "openai/gpt", label: "gpt", tag: "openai", detail: "", current: true }])
  await run("/agent")
  expect(pickers.at(-1)?.title).toBe("Agent")
  expect(pickers.at(-1)?.rows).toEqual([{ id: "build", label: "build", tag: "", detail: "Default agent", current: true }])
})

test("/model and /agent with no session and no argument remember the picker choice for the next session", async () => {
  const { store, calls, pickers, run } = harness()
  store.applyCatalog({
    sessions: [], interactions: [], models: [{ id: "openai/gpt", providerId: "openai", modelId: "gpt" }],
    agents: [{ name: "review" }], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  await run("/model")
  await pickers.at(-1)?.onSelect({ id: "openai/gpt", label: "gpt" })
  expect(store.state.pendingModel).toBe("openai/gpt")
  expect(store.state.status).toContain("applies when the session is created")
  await run("/agent")
  await pickers.at(-1)?.onSelect({ id: "review", label: "review" })
  expect(store.state.pendingAgent).toBe("review")
  expect(calls).toEqual([])
})

test("/model and /agent with an argument switch the session", async () => {
  const { store, run } = harness({
    updateSessionModel: async (session, model) => ({ id: session, agent: "build", workdir: "/w", model: { providerId: model.split("/")[0], modelId: model.split("/")[1] } }),
    updateSession: async (session, patch) => ({ id: session, agent: patch.agent ?? "build", workdir: "/w" }),
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/model anthropic/claude")
  expect(store.state.selected?.model).toEqual({ providerId: "anthropic", modelId: "claude" })
  await run("/agent review")
  expect(store.state.selected?.agent).toBe("review")
})

test("/rename updates the session title", async () => {
  const { store, run } = harness({
    updateSession: async (session, patch) => ({ id: session, agent: "build", workdir: "/w", title: patch.title }),
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/rename New title")
  expect(store.state.selected?.title).toBe("New title")
  expect(store.state.status).toBe("Renamed to New title")
  await expect(run("/rename   ")).rejects.toThrow("Usage: /rename <title> in a session")
})

test("/compact shows a compacting status then the outcome", async () => {
  const statuses: string[] = []
  const { store, run } = harness({
    compactSession: async () => ({ compactedUntilSeq: "10", strategy: "shake" }),
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  const original = store.setStatus
  store.setStatus = (text: string) => { statuses.push(text); original(text) }
  await run("/compact")
  expect(statuses).toEqual(["Compacting…", "Compacted · shake"])
})

test("/summarize refreshes messages after summarizing", async () => {
  const { store, calls, run } = harness({
    summarizeSession: async () => ({ summaryMessage: "msg_s" }),
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/summarize")
  expect(store.state.status).toBe("Summarized")
  expect(calls).toEqual(["refreshMessages"])
})

test("/todos loads the session todo list into the todos view", async () => {
  const { store, run } = harness({
    getSessionTodo: async () => [{ id: "t1", content: "Write tests", status: "TODO_STATUS_PENDING" }],
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  await run("/todos")
  expect(store.state.view).toBe("todos")
  expect(store.state.todos).toEqual([{ id: "t1", content: "Write tests", status: "TODO_STATUS_PENDING" }])
})

test("/status shows server, version, directory, session, agent, model, and mode", async () => {
  const { store, run } = harness()
  store.applyBootstrap({ location: { version: "0.42.0" } })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", title: "Fix bug", model: { providerId: "openai", modelId: "gpt" }, permissionMode: "yolo" })
  await run("/status")
  expect(store.state.view).toBe("status")
  const text = store.state.statusText
  expect(text).toContain("Version     0.42.0")
  expect(text).toContain("Session     Fix bug")
  expect(text).toContain("Agent       build")
  expect(text).toContain("Model       openai/gpt")
  expect(text).toContain("Mode        yolo")
  expect(text).not.toContain("WebUI")
})

test("/status shows the WebUI that bare hya serves, or why it is unavailable", async () => {
  const { store, run } = harness()
  store.setWeb({ url: "http://127.0.0.1:3250/" })
  await run("/status")
  expect(store.state.statusText).toContain("WebUI       http://127.0.0.1:3250")
  expect(store.state.statusText).toContain("Backend     in the hya process (bare hya)")
  store.setWeb({ error: "port 3250 is in use" })
  await run("/status")
  expect(store.state.statusText).toContain("WebUI       unavailable: port 3250 is in use · hya --port <N>")
})

test("argument completion comes from the command's own completer", () => {
  const { registry, store } = harness()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [{ name: "release" }], providers: [{ id: "openai" }], savedKeys: [], commands: [] })
  expect(registry.complete("/workflow run r", store.completionContext())).toEqual(["/workflow run release"])
  expect(registry.complete("/login o", store.completionContext())).toEqual(["/login openai"])
  expect(registry.complete("/unknown x", store.completionContext())).toEqual([])
})

test("/sidebar toggles or sets the sidebar and /thinking expands or collapses reasoning", async () => {
  const { store, run, registry } = harness()
  store.setColumns(80)
  await run("/sidebar")
  expect(store.state.sidebar).toBe("open")
  expect(store.state.status).toBe("Sidebar shown · Ctrl+B toggles")
  await run("/sidebar off")
  expect(store.state.sidebar).toBe("closed")
  expect(store.state.status).toBe("Sidebar hidden · Ctrl+B toggles")
  await run("/sidebar on")
  expect(store.state.sidebar).toBe("open")
  await expect(run("/sidebar maybe")).rejects.toThrow("Usage: /sidebar [on|off]")

  await run("/thinking")
  expect(store.state.thinking).toBe(true)
  expect(store.state.status).toBe("Reasoning expanded · Ctrl+O toggles")
  await run("/thinking off")
  expect(store.state.thinking).toBe(false)
  expect(store.state.status).toBe("Reasoning collapsed · Ctrl+O toggles")
  expect(registry.complete("/sidebar o", store.completionContext())).toEqual(["/sidebar off", "/sidebar on"])
})

test("/exit and /quit quit; /cancel cancels the running turn", async () => {
  const { calls, run } = harness()
  await run("/exit")
  await run("/quit")
  await run("/cancel")
  expect(calls).toEqual(["quit", "quit", "cancel"])
})

test("/permissions opens the mode picker from the backend listing; /permissions <mode> switches directly", async () => {
  const modes = [
    { id: "manual", title: "Manual", description: "Ask the user", source: "builtin" },
    { id: "yolo", title: "Yolo", description: "Allow everything", source: "builtin" },
    { id: "acme/approver/careful", title: "Careful", description: "Read-only commands", source: "acme/approver" },
  ]
  const { store, calls, pickers, run } = harness({ listPermissionModes: async () => modes })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", permissionMode: "acme/approver/careful" })
  await run("/permissions")
  expect(store.state.permissionModes).toEqual(modes)
  expect(pickers).toHaveLength(1)
  const picker = pickers[0]!
  expect(picker.title).toBe("Permission mode")
  expect(picker.rows.map((row) => [row.id, row.label, row.tag, row.current])).toEqual([
    ["manual", "Manual", "builtin", false],
    ["yolo", "Yolo", "builtin", false],
    ["acme/approver/careful", "Careful", "acme/approver", true],
  ])
  await picker.onSelect(picker.rows[1]!)
  await run("/permissions manual")
  expect(calls).toEqual(["mode yolo", "mode manual"])
})

test("/sessions opens a picker with a New session row first, then the tree, the open session marked", async () => {
  const { store, calls, pickers, run } = harness()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", title: "Top" })
  store.applyCatalog({
    sessions: [
      { id: "hysec_1", agent: "build", workdir: "/w", title: "Top" },
      { id: "hysec_2", agent: "review", workdir: "/w", parent: "hysec_1" },
    ],
    interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  await run("/sessions")
  expect(calls).toContain("refresh")
  const picker = pickers.at(-1)!
  expect(picker.title).toBe("Sessions")
  expect(picker.rows.map((row) => row.id)).toEqual(["__new__", "hysec_1", "hysec_2"])
  expect(picker.rows[1]?.current).toBe(true)
  expect(picker.actions?.map((action) => action.id)).toEqual(["rename", "delete"])
  // Enter on a row opens it; on the New session row it creates one.
  await picker.onSelect(picker.rows[2]!)
  expect(calls).toContain("open hysec_2")
  await picker.onSelect(picker.rows[0]!)
  expect(calls).toContain("new")
})

test("/sessions row actions: F2 renames (UpdateSession title), Ctrl+D deletes (DeleteSession) with confirmation already applied by the picker", async () => {
  const updateCalls: Array<{ id: string; patch: unknown }> = []
  const deleteCalls: string[] = []
  const { store, pickers, run } = harness({
    updateSession: async (id, patch) => { updateCalls.push({ id, patch }); return { id, agent: "build", workdir: "/w", title: (patch as { title?: string }).title } },
    deleteSession: async (id) => { deleteCalls.push(id) },
  })
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w", title: "Top" })
  store.applyCatalog({
    sessions: [{ id: "hysec_1", agent: "build", workdir: "/w", title: "Top" }],
    interactions: [], models: [], workflows: [], providers: [], savedKeys: [], commands: [],
  })
  await run("/sessions")
  const picker = pickers.at(-1)!
  await picker.onAction?.("rename", picker.rows[1]!, "New title")
  expect(updateCalls).toEqual([{ id: "hysec_1", patch: { title: "New title" } }])
  expect(store.state.selected?.title).toBe("New title")
  // Renaming reopens the picker so browsing continues.
  expect(pickers.length).toBeGreaterThan(1)

  await picker.onAction?.("delete", picker.rows[1]!)
  expect(deleteCalls).toEqual(["hysec_1"])
})
