import { expect, test } from "bun:test"
import { createMemo, createRoot } from "solid-js"
import { createAppStore } from "../src/state/store"

const session = (id: string, extra: Record<string, unknown> = {}) => ({ id, agent: "build", workdir: "/work", ...extra })

test("starts in the chat view with the startup status and no data", () => {
  const store = createAppStore()
  expect(store.state.ready).toBe(false)
  expect(store.state.view).toBe("chat")
  expect(store.state.status).toBe("Enter prompt · /help commands · Ctrl+R refresh · Ctrl+C quit")
  expect(store.state.sessions).toEqual([])
  expect(store.state.selected).toBeUndefined()
  expect(store.state.cursor).toBe("0")
  expect(store.state.savedKeysAvailable).toBe(true)
})

test("applies a catalog refresh and keeps the selected session in sync", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1", { title: "old" }))
  store.applyCatalog({
    sessions: [session("hysec_1", { title: "new" }), session("hysec_2")],
    interactions: [{ id: "req_1", type: "PERMISSION", title: "bash" }],
    models: [{ id: "hya/offline" }],
    workflows: [{ name: "release" }],
    providers: [{ id: "openai" }],
    savedKeys: null,
    commands: [{ name: "compact" }],
  })
  expect(store.state.ready).toBe(true)
  expect(store.state.selected?.title).toBe("new")
  expect(store.state.savedKeysAvailable).toBe(false)
  expect(store.state.savedKeys).toEqual([])
  expect(store.state.backendCommands.map((command) => command.name)).toEqual(["compact"])
  expect(store.state.interactions.map((item) => item.id)).toEqual(["req_1"])
})

test("opening a session resets the transcript and resumes from its last sequence", () => {
  const store = createAppStore()
  store.setView("models")
  store.openSession(session("hysec_1", { lastSeq: "42" }))
  expect(store.state.view).toBe("chat")
  expect(store.state.cursor).toBe("42")
  expect(store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_USER" }])).toBe(true)
  expect(store.state.messages.map((message) => message.id)).toEqual(["m1"])
  store.openSession(session("hysec_2"))
  expect(store.state.messages).toEqual([])
  expect(store.state.cursor).toBe("0")
  expect(store.setMessages("hysec_1", [{ id: "late", role: "ROLE_USER" }])).toBe(false)
  expect(store.state.messages).toEqual([])
})

test("advances the stream cursor only forward and tracks the turn state", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  store.applyEvent({ seq: "10", session: "hysec_1", messageStarted: { message: "m1", role: "ROLE_USER" } })
  store.applyEvent({ seq: "9", session: "hysec_1", messageStarted: { message: "m0", role: "ROLE_USER" } })
  store.applyEvent({ session: "hysec_1", partStarted: { message: "m2", part: "p", kind: "text" } })
  expect(store.state.cursor).toBe("10")
  store.beginTurn()
  expect(store.state.running).toBe(true)
  store.setTurn("msg_1")
  expect(store.state.turnId).toBe("msg_1")
  store.endTurn()
  expect(store.state.running).toBe(false)
  expect(store.state.turnId).toBe("")
})

test("tracks concealed key entry by provider and mask only", () => {
  const store = createAppStore()
  store.beginSecret("openai")
  store.setSecretMask("•••")
  expect(store.state.secretProvider).toBe("openai")
  expect(store.state.secretMask).toBe("•••")
  store.endSecret()
  expect(store.state.secretProvider).toBeUndefined()
  expect(store.state.secretMask).toBe("")
})

test("builds the completion context from the current catalogs", () => {
  const store = createAppStore()
  store.applyBootstrap({ agents: [{ name: "build" }], models: [{ id: "hya/offline" }], interactions: [] })
  store.applyCatalog({
    sessions: [session("hysec_1")], interactions: [], models: [{ id: "hya/offline" }], workflows: [{ name: "release" }],
    providers: [{ id: "openai" }], savedKeys: ["anthropic", "openai"], commands: [{ name: "compact" }],
  })
  const context = store.completionContext()
  expect(context.providers).toEqual(["openai", "anthropic"])
  expect(context.savedKeys).toEqual(["anthropic", "openai"])
  expect(context.agents).toEqual(["build"])
  expect(context.sessions).toEqual(["hysec_1"])
  expect(context.backendCommands).toEqual(["compact"])
  expect(context.apiOperations).toContain("GET /v1/health")
})

test("notifies Solid computations when state changes", () => {
  createRoot((dispose) => {
    const store = createAppStore()
    const title = createMemo(() => store.state.view)
    expect(title()).toBe("chat")
    store.setView("help")
    expect(title()).toBe("help")
    dispose()
  })
})

test("reasoning is collapsed by default; the global switch and per-part toggles expand it", async () => {
  const { reasoningExpanded } = await import("../src/state/messages")
  const store = createAppStore()
  expect(store.state.thinking).toBe(false)
  expect(reasoningExpanded(store.state, "r1")).toBe(false)
  store.toggleReasoning("r1")
  expect(reasoningExpanded(store.state, "r1")).toBe(true)
  expect(reasoningExpanded(store.state, "r2")).toBe(false)
  // The global switch expands everything and forgets per-part choices.
  store.setThinking(true)
  expect(reasoningExpanded(store.state, "r1")).toBe(true)
  expect(reasoningExpanded(store.state, "r2")).toBe(true)
  store.toggleReasoning("r2")
  expect(reasoningExpanded(store.state, "r2")).toBe(false)
  store.setThinking(false)
  expect(reasoningExpanded(store.state, "r2")).toBe(false)
  expect(reasoningExpanded(store.state, "r1")).toBe(false)
})

test("submitting a prompt asks the transcript to jump to the newest line", () => {
  const store = createAppStore()
  const before = store.state.followTick
  store.followTranscript()
  expect(store.state.followTick).toBe(before + 1)
})

test("interaction frames add, enrich, and resolve pending asks; answered ids stay hidden", () => {
  const store = createAppStore()
  store.openSession(session("hysec_1"))
  const perm = { id: "perm_1", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION", title: "bash ls", payload: { callId: "call_1" } }
  const ask = { id: "que_1", session: "hysec_1", type: "INTERACTION_TYPE_QUESTION", title: "Color?", detail: "Color", options: ["red", "blue"] }
  store.applyEvent({ session: "hysec_1", permissionRequested: { interaction: perm } })
  store.applyEvent({ session: "hysec_1", questionRequested: { interaction: ask } })
  store.applyEvent({ session: "hysec_1", permissionRequested: { interaction: perm } })
  expect(store.state.interactions.map((row) => row.id)).toEqual(["perm_1", "que_1"])
  // The listing omits a question's options: the frame's are kept.
  store.setInteractions([perm, { id: "que_1", session: "hysec_1", type: "INTERACTION_TYPE_QUESTION", title: "Color?" }])
  expect(store.state.interactions[1]).toMatchObject({ options: ["red", "blue"], detail: "Color" })
  // Resolved elsewhere (another client, a yolo switch).
  store.applyEvent({ session: "hysec_1", interactionResolved: { request: "perm_1" } })
  expect(store.state.interactions.map((row) => row.id)).toEqual(["que_1"])
  // Answered here: hidden at once, and a stale listing does not bring it back.
  store.resolveInteraction("que_1")
  expect(store.state.interactions).toEqual([])
  store.setInteractions([ask])
  expect(store.state.interactions).toEqual([])
  // A failed answer shows it again.
  store.unresolveInteraction("que_1")
  store.setInteractions([ask])
  expect(store.state.interactions.map((row) => row.id)).toEqual(["que_1"])
})

test("the highlighted prompt option belongs to one ask; the draft flag follows the input", () => {
  const store = createAppStore()
  expect(store.promptIndex("perm_1")).toBe(0)
  store.setPromptIndex("perm_1", 2)
  expect(store.promptIndex("perm_1")).toBe(2)
  expect(store.promptIndex("perm_2")).toBe(0)
  expect(store.state.draft).toBe(false)
  store.setDraft(true)
  expect(store.state.draft).toBe(true)
})
