import { expect, test } from "bun:test"
import { builtinPreset, statusSegments } from "../src/config/presets"
import { chatPaneLayout } from "../src/state/panes"
import { borderInfoText, contextBar, fitStatusLine, statusLine, statusLineShown, statusLineText, statusPart, type StatusPart } from "../src/state/statusline"
import { createAppStore } from "../src/state/store"

const env = { HOME: "/home/u" }

const open = (extra: Record<string, unknown> = {}) => {
  const store = createAppStore()
  store.applyCatalog({
    sessions: [], interactions: [], providers: [], commands: [],
    models: [{ id: "fake/model", contextLimit: "1000" }],
  })
  store.openSession({ id: "hysec_1", agent: "hya-main", workdir: "/home/u/src/work", model: { providerId: "fake", modelId: "model" }, ...extra })
  return store
}

test("the ctx bar fills a cell per 20%, at least one above 0%", () => {
  expect(contextBar(0)).toBe("─────")
  expect(contextBar(1)).toBe("━────")
  expect(contextBar(42)).toBe("━━───")
  expect(contextBar(100)).toBe("━━━━━")
  expect(contextBar(140)).toBe("━━━━━")
})

test("segments read the session, mode, usage, and workspace; empty facts are omitted", () => {
  const store = open({ title: "Fix tests", effectiveEffort: "high", usage: { input: "12000", output: "300" } })
  expect(statusPart(store.state, "model", env)).toEqual({ id: "model", text: "model · high", tone: "fg" })
  expect(statusPart(store.state, "mode", env)).toEqual({ id: "mode", text: "manual", tone: "muted" })
  expect(statusPart(store.state, "agent", env)?.text).toBe("hya-main")
  expect(statusPart(store.state, "cwd", env)?.text).toBe("~/src/work")
  expect(statusPart(store.state, "tokens", env)?.text).toBe("12.3k tok")
  expect(statusPart(store.state, "session", env)?.text).toBe("Fix tests")
  // No usage yet, no branch, connected: nothing to show.
  expect(statusPart(store.state, "ctx", env)).toBeUndefined()
  expect(statusPart(store.state, "git", env)).toBeUndefined()
  store.setConnected(true)
  expect(statusPart(store.state, "connection", env)).toBeUndefined()
  store.markReady()
  store.setConnected(false)
  expect(statusPart(store.state, "connection", env)).toEqual({ id: "connection", text: "reconnecting", tone: "warning" })
  store.setBackendStopped(true)
  expect(statusPart(store.state, "connection", env)).toEqual({ id: "connection", text: "backend stopped", tone: "error" })
})

test("ctx warns from 80% and alarms from 95%; yolo is the error color", () => {
  const store = open()
  const round = (input: string) => store.setMessages("hysec_1", [{ id: "m1", role: "ROLE_ASSISTANT", model: "fake/model", roundUsage: { input } }])
  round("420")
  expect(statusPart(store.state, "ctx", env)).toEqual({ id: "ctx", text: "ctx ━━─── 42%", tone: "muted" })
  round("800")
  expect(statusPart(store.state, "ctx", env)?.tone).toBe("warning")
  round("950")
  expect(statusPart(store.state, "ctx", env)?.tone).toBe("error")
  store.setSelected({ ...store.state.selected!, permissionMode: "yolo" })
  expect(statusPart(store.state, "mode", env)).toEqual({ id: "mode", text: "⚠ yolo", tone: "error" })
})

test("the project name stands for the directory when a Project is active", () => {
  const store = open()
  store.setProjects([{ id: "p1", name: "hya", roots: ["/home/u/src/work"] }])
  store.setActiveProject("p1")
  expect(statusPart(store.state, "cwd", env)?.text).toBe("hya")
})

const part = (id: StatusPart["id"], text: string): StatusPart => ({ id, text, tone: "muted" })

test("fitting drops session, tokens, git, cwd, agent in that order, then cuts the model", () => {
  const parts = [part("model", "gpt-6.1 · med"), part("mode", "manual"), part("agent", "hya-main"), part("cwd", "hya"), part("git", "⎇ main"), part("ctx", "ctx ━━─── 42%"), part("tokens", "12k tok"), part("session", "Fix the tests")]
  const ids = (columns: number) => {
    const line = fitStatusLine(parts, columns)
    return [...line.left.map((item) => item.id), ...(line.right ? [`>${line.right.id}`] : [])]
  }
  expect(ids(200)).toEqual(["model", "mode", "agent", "cwd", "git", "ctx", "tokens", ">session"])
  expect(ids(80)).toEqual(["model", "mode", "agent", "cwd", "git", "ctx", "tokens"])
  expect(ids(70)).toEqual(["model", "mode", "agent", "cwd", "git", "ctx"])
  expect(ids(60)).toEqual(["model", "mode", "agent", "cwd", "ctx"])
  expect(ids(50)).toEqual(["model", "mode", "agent", "ctx"])
  expect(ids(40)).toEqual(["model", "mode", "ctx"])
  // Below that only the model name shrinks; model, mode, and ctx never go.
  const narrow = fitStatusLine(parts, 30)
  expect(narrow.left.map((item) => item.id)).toEqual(["model", "mode", "ctx"])
  expect(statusLineText(narrow, 30)).toBe("gpt-… · manual · ctx ━━─── 42%")
  expect(Bun.stringWidth(statusLineText(narrow, 30))).toBe(30)
  // The session title goes to the right end with at least two spaces.
  expect(statusLineText(fitStatusLine([part("mode", "manual"), part("session", "S")], 12), 12)).toBe("manual     S")
})

test("statusLine keeps the configured order and skips empty segments", () => {
  const store = open({ usage: { input: "100" } })
  const line = statusLine(store.state, [...statusSegments], 200, env)
  expect(line.left.map((item) => item.id)).toEqual(["model", "mode", "agent", "cwd", "tokens"])
  expect(line.right?.text).toBe("hysec_1")
  expect(borderInfoText(store.state, ["model", "mode"], 80, env)).toBe(" model · manual ")
  expect(borderInfoText(store.state, ["model", "mode"], 6, env)).toBe(" mode…")
  expect(borderInfoText(store.state, [], 80, env)).toBe("")
})

test("the status line hides when off, and while a visible Context pane shows the same facts", () => {
  const store = createAppStore()
  // The store starts in the workspace structure: at 190 columns the Context pane shows.
  expect(statusLineShown(store.state, 190)).toBe(false)
  expect(statusLineShown(store.state, 120)).toBe(true)
  store.setSidebar("closed")
  expect(statusLineShown(store.state, 190)).toBe(true)
  store.setStructure(builtinPreset("statusline"))
  store.setPaneLayout(chatPaneLayout())
  expect(statusLineShown(store.state, 190)).toBe(true)
  store.setStructure(builtinPreset("minimal"))
  expect(statusLineShown(store.state, 190)).toBe(false)
})
