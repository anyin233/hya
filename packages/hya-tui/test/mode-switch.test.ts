import { expect, test } from "bun:test"
import { createModeSwitcher } from "../src/app/modes"
import { HyaClient, type FetchLike, type Interaction, type SessionInfo } from "../src/client"
import { createAppStore } from "../src/state/store"
import { transcriptViews } from "../src/state/messages"

const key = (name: string, extra: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; sequence: string }> = {}) =>
  ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra })

const session: SessionInfo = { id: "hysec_1", agent: "build", workdir: "/w", permissionMode: "manual" }
const ask: Interaction = { id: "perm_1", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION", title: "bash echo hi" } as Interaction

function harness(options: { interactions?: Interaction[][]; fail?: boolean } = {}) {
  const store = createAppStore()
  const calls: Array<{ method: string; body?: unknown }> = []
  const listings = [...(options.interactions ?? [])]
  const client = {
    async updateSession(id: string, patch: { permissionMode?: string }) {
      calls.push({ method: `PATCH ${id}`, body: patch })
      if (options.fail) throw new Error("invalid_argument: unknown permission mode")
      return { ...session, id, permissionMode: patch.permissionMode }
    },
    async listInteractions() {
      calls.push({ method: "listInteractions" })
      return listings.shift() ?? []
    },
  }
  const modes = createModeSwitcher({ store, client })
  return { store, calls, modes }
}

const notices = (store: ReturnType<typeof createAppStore>): string[] =>
  transcriptViews(store.state).filter((view) => view.role === "divider").map((view) => view.blocks.map((block) => block.kind === "text" ? block.text : "").join(""))

test("the client sends UpdateSession with permissionMode and lists the permission modes", async () => {
  const calls: Array<{ url: string; method: string; body: unknown }> = []
  const fetcher: FetchLike = async (input, init) => {
    calls.push({ url: String(input), method: init?.method ?? "GET", body: init?.body ? JSON.parse(String(init.body)) : undefined })
    return Response.json(String(input).endsWith("/permission-modes")
      ? { modes: [{ id: "manual", title: "Manual", description: "Ask", source: "builtin" }] }
      : { ...session, permissionMode: "yolo" })
  }
  const client = new HyaClient("http://127.0.0.1:8080/", "/w", fetcher)
  expect((await client.updateSession("hysec_1", { permissionMode: "yolo" })).permissionMode).toBe("yolo")
  expect(await client.listPermissionModes()).toEqual([{ id: "manual", title: "Manual", description: "Ask", source: "builtin" }])
  expect(calls).toEqual([
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1", method: "PATCH", body: { permissionMode: "yolo" } },
    { url: "http://127.0.0.1:8080/v1/permission-modes", method: "GET", body: undefined },
  ])
})

test("an older backend without the listing route yields no modes", async () => {
  const fetcher: FetchLike = async () => Response.json({ error: { code: "not_found", message: "no route" } }, { status: 404 })
  expect(await new HyaClient("http://x/", "/w", fetcher).listPermissionModes()).toEqual([])
})

test("Shift+Tab to yolo asks first; Enter applies it, re-lists interactions, and adds a notice", async () => {
  const { store, calls, modes } = harness({ interactions: [[]] })
  store.openSession(session)
  modes.cycle()
  expect(store.state.modeConfirm).toEqual({ target: "yolo", from: "manual" })
  expect(calls).toEqual([])
  expect(modes.key(key("return"))).toBe(true)
  await modes.idle()
  expect(store.state.modeConfirm).toBeUndefined()
  expect(calls).toEqual([{ method: "PATCH hysec_1", body: { permissionMode: "yolo" } }, { method: "listInteractions" }])
  expect(store.state.selected?.permissionMode).toBe("yolo")
  expect(notices(store)).toEqual(["Permission mode → yolo"])
  // Confirmed once per process: the next switch to yolo applies at once.
  modes.cycle()
  await modes.idle()
  expect(store.state.selected?.permissionMode).toBe("manual")
  modes.cycle()
  expect(store.state.modeConfirm).toBeUndefined()
  await modes.idle()
  expect(store.state.selected?.permissionMode).toBe("yolo")
  expect(notices(store)).toEqual(["Permission mode → yolo", "Permission mode → manual", "Permission mode → yolo"])
})

test("switching to yolo closes the pending asks the backend allowed", async () => {
  const { store, modes } = harness({ interactions: [[]] })
  store.openSession(session)
  store.setInteractions([ask])
  expect(store.state.interactions.map((row) => row.id)).toEqual(["perm_1"])
  await modes.request("yolo", { confirmed: true })
  expect(store.state.interactions).toEqual([])
})

test("Esc cancels the yolo confirmation and nothing is sent", () => {
  const { store, calls, modes } = harness()
  store.openSession(session)
  modes.cycle()
  expect(modes.key(key("escape"))).toBe(true)
  expect(store.state.modeConfirm).toBeUndefined()
  expect(store.state.selected?.permissionMode).toBe("manual")
  expect(calls).toEqual([])
  expect(store.state.status).toBe("Permission mode unchanged · manual")
})

test("another key cancels the confirmation and is not consumed", () => {
  const { store, modes } = harness()
  store.openSession(session)
  modes.cycle()
  expect(modes.key(key("a"))).toBe(false)
  expect(store.state.modeConfirm).toBeUndefined()
  // Without a confirmation shown, the switcher takes no keys.
  expect(modes.key(key("return"))).toBe(false)
})

test("before a session exists the choice is remembered and applied right after the session is created", async () => {
  const { store, calls, modes } = harness({ interactions: [[]] })
  await modes.request("acme/approver/careful")
  expect(calls).toEqual([])
  expect(store.state.pendingMode).toBe("acme/approver/careful")
  expect(modes.current()).toBe("acme/approver/careful")
  expect(store.state.status).toBe("Permission mode → acme/approver/careful · applies when the session is created")
  store.openSession(session)
  await modes.applyPending()
  expect(calls[0]).toEqual({ method: "PATCH hysec_1", body: { permissionMode: "acme/approver/careful" } })
  expect(store.state.pendingMode).toBeUndefined()
  expect(store.state.selected?.permissionMode).toBe("acme/approver/careful")
  // Nothing pending: applying again sends nothing.
  await modes.applyPending()
  expect(calls.filter((call) => call.method.startsWith("PATCH"))).toHaveLength(1)
})

test("a rejected mode keeps the old one and reports the error", async () => {
  const { store, modes } = harness({ fail: true })
  store.openSession(session)
  await modes.request("bogus")
  expect(store.state.selected?.permissionMode).toBe("manual")
  expect(store.state.status).toContain("Permission mode failed: ")
  expect(notices(store)).toEqual([])
})

test("a sessionUpdated frame with a new mode updates the session and adds one notice", () => {
  const store = createAppStore()
  store.openSession(session)
  store.applyEvent({ seq: "5", session: "hysec_1", sessionUpdated: { permissionMode: "yolo" } })
  expect(store.state.selected?.permissionMode).toBe("yolo")
  // The same mode again (our own PATCH echoed) adds no second notice.
  store.applyPermissionMode("yolo")
  store.applyEvent({ seq: "6", session: "hysec_1", sessionUpdated: { permissionMode: "yolo" } })
  expect(notices(store)).toEqual(["Permission mode → yolo"])
  // A frame of another session is ignored.
  store.applyEvent({ seq: "7", session: "hysec_other", sessionUpdated: { permissionMode: "manual" } })
  expect(store.state.selected?.permissionMode).toBe("yolo")
})

test("a notice added while the transcript is empty stays above later messages", () => {
  const store = createAppStore()
  store.openSession(session)
  store.applyPermissionMode("yolo")
  store.setMessages("hysec_1", [{ id: "msg_1", role: "ROLE_USER", parts: [{ id: "p1", text: { text: "hi" } }] }] as never)
  store.applyPermissionMode("manual")
  expect(transcriptViews(store.state).map((view) => view.role === "divider" ? view.blocks.map((block) => block.kind === "text" ? block.text : "").join("") : view.id))
    .toEqual(["Permission mode → yolo", "msg_1", "Permission mode → manual"])
})
