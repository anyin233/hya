import { expect, test } from "bun:test"
import type { HyaClient } from "../src/client"
import { nativeCommands } from "../src/completion"
import { createCommandRegistry, type AppActions } from "../src/commands"
import { createAppStore } from "../src/state/store"
import type { PickerSpec } from "../src/state/picker"
import { colors, defaultThemeName, setTheme, themeName, themes } from "../src/theme"

function harness(client: Partial<HyaClient> = {}, copyWorks = true) {
  const store = createAppStore()
  const calls: string[] = []
  const pickers: PickerSpec[] = []
  const actions: AppActions = {
    refresh: async () => { calls.push("refresh") },
    refreshMessages: async () => { calls.push("refreshMessages") },
    openSession: async (id) => { calls.push(`open ${id}`) },
    newSession: async (agent, model) => { calls.push(`new ${agent ?? ""} ${model ?? ""}`.trim()) },
    newTemporarySession: async () => { calls.push("new temp") },
    switchProject: async (id) => { calls.push(`project ${id}`) },
    refreshProjects: async () => { calls.push("projects") },
    openProviders: () => { calls.push("providers") },
    openDiff: () => { calls.push("diff") },
    openMcp: () => { calls.push("mcp") },
    openRules: () => { calls.push("rules") },
    openAgentModels: () => { calls.push("agentModels") },
    scheduleRefresh: () => { calls.push("scheduleRefresh") },
    cancelTurn: async () => { calls.push("cancel") },
    quit: () => { calls.push("quit") },
    openPicker: (picker) => { pickers.push(picker) },
    requestPermissionMode: async (mode) => { calls.push(`mode ${mode}`) },
    openHelp: () => { calls.push("help") },
    savePreferences: (patch) => { calls.push(`prefs ${JSON.stringify(patch)}`) },
    copyText: (text) => { calls.push(`copy ${text}`); return copyWorks },
    openEditor: () => { calls.push("editor") },
    undo: async () => { calls.push("undo") },
    redo: async () => { calls.push("redo") },
    fork: () => { calls.push("fork") },
  }
  const registry = createCommandRegistry()
  const context = { store, client: client as HyaClient, actions }
  return { store, calls, pickers, registry, run: (text: string) => registry.dispatch(text, context) }
}

test("registers every native slash command with a description", () => {
  const { registry } = harness()
  expect(registry.names().sort()).toEqual([...nativeCommands].sort())
  for (const name of registry.names()) expect(registry.get(name)?.description.length).toBeGreaterThan(0)
  // `/key` opens the Provider View and takes no arguments; `/keys` and `/login` are gone.
  expect(registry.get("/key")?.argumentHint).toBeUndefined()
  expect(registry.get("/key")?.complete).toBeUndefined()
  expect(registry.get("/keys")).toBeUndefined()
  expect(registry.get("/login")).toBeUndefined()
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

test("/open resolves list numbers and /key opens the Provider View", async () => {
  const { store, calls, run } = harness()
  store.applyCatalog({ sessions: [{ id: "hysec_a", agent: "build", workdir: "/w" }, { id: "hysec_b", agent: "build", workdir: "/w" }], interactions: [], models: [], workflows: [], providers: [], commands: [] })
  await run("/open 2")
  await run("/open hysec_x")
  await run("/key")
  expect(calls).toEqual(["open hysec_b", "open hysec_x", "providers"])
})

test("/diff, /mcp, /rules, /agent-models open their full-screen views", async () => {
  const { calls, run } = harness()
  await run("/diff")
  await run("/mcp")
  await run("/rules")
  await run("/agent-models")
  expect(calls).toEqual(["diff", "mcp", "rules", "agentModels"])
})

test("usage errors are thrown for incomplete commands", async () => {
  const { run } = harness()
  await expect(run("/open")).rejects.toThrow("Usage: /open <session id or number>")
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
    agents: [{ name: "build", description: "Default agent" }], workflows: [], providers: [], commands: [],
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
    agents: [{ name: "review" }], workflows: [], providers: [], commands: [],
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

test("/status says whether the backend was started by this TUI or attached to a running server", async () => {
  const { store, run } = harness()
  store.setBackend({ pid: 11, bin: "/b/hya", db: "/s/sessions.db" })
  await run("/status")
  expect(store.state.statusText).toContain("Backend     started by this TUI · pid 11 · /b/hya · db /s/sessions.db")
  store.setBackend({ pid: 22, db: "/s/sessions.db", attached: true })
  await run("/status")
  expect(store.state.statusText).toContain("Backend     attached to a running server · pid 22 · db /s/sessions.db")
  // Bare hya that attached passes only the pid.
  store.setWeb({ url: "http://127.0.0.1:3250/" })
  store.setBackend({ pid: 33, attached: true })
  await run("/status")
  expect(store.state.statusText).toContain("Backend     attached to a running server · pid 33")
  expect(store.state.statusText).not.toContain("in the hya process")
})

test("argument completion comes from the command's own completer", () => {
  const { registry, store } = harness()
  store.applyCatalog({ sessions: [], interactions: [], models: [], workflows: [{ name: "release" }], providers: [{ id: "openai" }], commands: [] })
  expect(registry.complete("/workflow run r", store.completionContext())).toEqual(["/workflow run release"])
  expect(registry.complete("/key o", store.completionContext())).toEqual([])
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
    interactions: [], models: [], workflows: [], providers: [], commands: [],
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
    interactions: [], models: [], workflows: [], providers: [], commands: [],
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

test("/theme opens a picker of the built-in themes with live preview; Enter persists, Esc restores", async () => {
  const { calls, pickers, run, store } = harness()
  try {
    await run("/theme")
    const picker = pickers.at(-1)!
    expect(picker.title).toBe("Theme")
    expect(picker.rows.map((row) => row.id)).toEqual(Object.keys(themes))
    expect(picker.rows.find((row) => row.current)?.id).toBe("hya")
    expect(picker.rows.find((row) => row.id === "light")?.tag).toBe("light")

    // Moving the highlight previews; Esc (cancel) restores the theme in effect before.
    picker.onHighlight!(picker.rows.find((row) => row.id === "light")!)
    expect(themeName()).toBe("light")
    expect(colors.bg).toBe(themes.light.colors.bg)
    picker.onCancel!()
    expect(themeName()).toBe("hya")
    expect(calls.filter((call) => call.startsWith("prefs"))).toEqual([])

    // Enter keeps the theme and saves it to the preferences file.
    await run("/theme")
    const again = pickers.at(-1)!
    again.onHighlight!(again.rows.find((row) => row.id === "contrast")!)
    await again.onSelect(again.rows.find((row) => row.id === "light")!)
    expect(themeName()).toBe("light")
    expect(calls).toContain('prefs {"theme":"light"}')
    expect(store.state.status).toContain("Theme → Light")

    // The next picker marks the theme now in effect.
    await run("/theme")
    expect(pickers.at(-1)!.rows.find((row) => row.current)?.id).toBe("light")
  } finally {
    setTheme(defaultThemeName)
  }
})

test("/copy copies the last assistant reply's text through OSC 52 and says how much", async () => {
  const { store, calls, run } = harness()
  await run("/copy")
  expect(calls).toEqual([])
  expect(store.state.status).toBe("Nothing to copy: no assistant reply yet")
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [
    { id: "u", role: "ROLE_USER", finish: "FINISH_REASON_STOP", parts: [{ id: "p1", text: { text: "hi" } }] },
    { id: "a1", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p2", text: { text: "first" } }] },
    { id: "a2", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p3", text: { text: "Hello **there**" } }, { id: "p4", text: { text: "second part" } }] },
  ])
  await run("/copy")
  expect(calls).toEqual(["copy Hello **there**\n\nsecond part"])
  expect(store.state.status).toBe("Copied 28 chars")
})

test("/copy reports a terminal without OSC 52", async () => {
  const { store, run } = harness({}, false)
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setMessages("hysec_1", [{ id: "a1", role: "ROLE_ASSISTANT", finish: "FINISH_REASON_STOP", parts: [{ id: "p", text: { text: "x" } }] }])
  await run("/copy")
  expect(store.state.status).toBe("Copy failed: this terminal does not accept OSC 52 clipboard writes")
})

test("/editor opens the external editor on the input", async () => {
  const { calls, run } = harness()
  await run("/editor")
  expect(calls).toEqual(["editor"])
})

test("/vim toggles vim mode (or sets it with on/off) and saves it", async () => {
  const { store, calls, run } = harness()
  expect(store.state.vim).toBe(false)
  await run("/vim")
  expect(store.state.vim).toBe(true)
  expect(store.state.vimMode).toBe("insert")
  expect(store.state.status).toBe("Vim mode on · Esc for normal mode, i to insert")
  await run("/vim on")
  expect(store.state.vim).toBe(true)
  await run("/vim off")
  expect(store.state.vim).toBe(false)
  expect(store.state.status).toBe("Vim mode off")
  expect(calls).toEqual(['prefs {"vim":true}', 'prefs {"vim":true}', 'prefs {"vim":false}'])
  await expect(run("/vim maybe")).rejects.toThrow("Usage: /vim [on|off]")
})

test("/notifications toggles desktop notifications (or sets it with on/off) and saves it", async () => {
  const { store, calls, run } = harness()
  expect(store.state.notifications).toBe(true)
  await run("/notifications off")
  expect(store.state.notifications).toBe(false)
  expect(store.state.status).toBe("Desktop notifications off")
  await run("/notifications on")
  expect(store.state.notifications).toBe(true)
  expect(store.state.status).toBe("Desktop notifications on")
  await run("/notifications")
  expect(store.state.notifications).toBe(false)
  expect(calls).toEqual(['prefs {"notifications":false}', 'prefs {"notifications":true}', 'prefs {"notifications":false}'])
  await expect(run("/notifications maybe")).rejects.toThrow("Usage: /notifications [on|off]")
})

test("/undo, /redo, and /fork run the revert actions", async () => {
  const { calls, run } = harness()
  await run("/undo")
  await run("/redo")
  await run("/fork")
  expect(calls).toEqual(["undo", "redo", "fork"])
})

test("/status names the session a fork came from", async () => {
  const { store, run } = harness()
  store.setSessions([{ id: "hysec_src", agent: "build", workdir: "/w", title: "Parser" }])
  store.openSession({ id: "hysec_2", agent: "build", workdir: "/w", forkedFrom: { session: "hysec_src" } })
  await run("/status")
  expect(store.state.statusText).toContain("Forked      from Parser")
})
