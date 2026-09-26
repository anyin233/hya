// Archive on graceful exit, `/to-background`, `--resume` / `/resume`, and the
// archived toggle of `/sessions` (docs/tui.md "Sessions on start and exit").
import { expect, test } from "bun:test"
import type { HyaClient, SessionInfo } from "../src/client"
import { createCommandRegistry, type AppActions } from "../src/commands"
import { completeCommand } from "../src/completion"
import { createResumer } from "../src/app/resume"
import { initialSessionId } from "../src/launch"
import { resumeRows, sessionRows } from "../src/state/catalog"
import { sessionListText } from "../src/state/format"
import { pickerKey, createPicker, type PickerSpec } from "../src/state/picker"
import { createAppStore } from "../src/state/store"
import { sessionPickerActions, toBackground } from "../src/commands/native"
import { webTabBackgroundNotice } from "../src/state/format"

const now = Date.parse("2026-09-26T12:00:00Z")

const sessions: SessionInfo[] = [
  { id: "old", agent: "build", workdir: "/w", title: "Old work", timeUpdated: "2026-09-20T12:00:00Z" },
  { id: "arch", agent: "build", workdir: "/w", title: "Archived work", archived: true, timeUpdated: "2026-09-26T11:00:00Z" },
  { id: "child", agent: "explore", workdir: "/w", parent: "old", timeUpdated: "2026-09-26T11:59:00Z" },
  { id: "new", agent: "build", workdir: "/w", timeUpdated: "2026-09-26T11:30:00Z" },
  { id: "elsewhere", agent: "build", workdir: "/other", timeUpdated: "2026-09-26T11:58:00Z" },
]

test("resumeRows: the directory's root sessions incl. archived, newest first, archived marked", () => {
  const rows = resumeRows(sessions, "/w", "old", now)
  expect(rows.map((row) => row.id)).toEqual(["new", "arch", "old"])
  expect(rows.find((row) => row.id === "arch")?.tag).toBe("archived")
  expect(rows.find((row) => row.id === "new")?.tag).toBe("")
  expect(rows.find((row) => row.id === "old")?.current).toBe(true)
  expect(rows.find((row) => row.id === "arch")?.detail).toContain("1h")
})

test("sessionRows marks archived sessions; --continue never picks an archived one", () => {
  const rows = sessionRows(sessions, undefined, now)
  expect(rows.find((row) => row.id === "arch")?.tag).toBe("archived")
  expect(rows.find((row) => row.id === "child")?.tag).toBe("subagent")
  expect(initialSessionId(sessions, { continue: true }, "/w")).toBe("new")
})

test("a sessionUpdated archived frame drops the sidebar row (the open one stays, marked) and an unarchive marks it back", () => {
  const store = createAppStore()
  store.applyCatalog({ sessions: sessions.filter((row) => !row.archived), interactions: [], models: [], workflows: [], providers: [], commands: [] })
  store.openSession({ id: "new", agent: "build", workdir: "/w" })
  store.applyEvent({ seq: "5", session: "old", sessionUpdated: { archived: true } })
  expect(store.state.sessions.map((row) => row.id)).not.toContain("old")
  store.applyEvent({ seq: "6", session: "new", sessionUpdated: { archived: true } })
  expect(store.state.sessions.find((row) => row.id === "new")?.archived).toBe(true)
  expect(store.state.selected?.archived).toBe(true)
  expect(sessionListText(store.state)).toContain("archived")
  store.applyEvent({ seq: "7", session: "new", sessionUpdated: { archived: false } })
  expect(store.state.selected?.archived).toBe(false)
  expect(sessionListText(store.state)).not.toContain("archived")
})

function harness(options: { webTab?: boolean } = {}) {
  const store = createAppStore()
  if (options.webTab) store.setWebTab(true)
  const calls: string[] = []
  const pickers: PickerSpec[] = []
  const actions = {
    refresh: async () => { calls.push("refresh") },
    openSession: async (id: string) => { calls.push(`open ${id}`) },
    newSession: async () => { calls.push("new") },
    openPicker: (picker: PickerSpec) => { pickers.push(picker) },
    quit: (mode?: string) => { calls.push(`quit ${mode}`) },
    resume: async (id?: string) => { calls.push(`resume ${id ?? ""}`.trim()) },
  } as unknown as AppActions
  const archivedFlags: boolean[] = []
  const client = {
    listSessions: async (opts?: { includeArchived?: boolean }) => { archivedFlags.push(opts?.includeArchived === true); return opts?.includeArchived ? sessions : sessions.filter((row) => !row.archived) },
  } as unknown as HyaClient
  const registry = createCommandRegistry()
  const context = { store, client, actions }
  return { store, calls, pickers, archivedFlags, registry, run: (text: string) => registry.dispatch(text, context) }
}

test("/exit and /quit quit and archive; /to-background quits and keeps the session running", async () => {
  const { calls, run } = harness()
  await run("/exit")
  await run("/quit")
  await run("/to-background")
  expect(calls).toEqual(["quit archive", "quit archive", "quit background"])
})

test("in a WebUI tab /to-background is hidden and only explains that closing the tab keeps the session running", async () => {
  const { store, calls, registry, run } = harness({ webTab: true })
  await run("/to-background")
  expect(calls).toEqual([])
  expect(store.state.status).toBe(webTabBackgroundNotice)
  expect(webTabBackgroundNotice).toBe("Close the tab to leave this session running")
  expect(completeCommand("/to-b", store.completionContext(), registry)).toEqual([])
  const terminal = harness()
  expect(completeCommand("/to-b", terminal.store.completionContext(), terminal.registry)).toEqual(["/to-background"])
})

test("/resume [id] resumes a session; /sessions toggles archived sessions with Ctrl+A and resumes an archived pick", async () => {
  const { calls, pickers, archivedFlags, run } = harness()
  await run("/resume")
  await run("/resume hysec_9")
  expect(calls).toEqual(["resume", "resume hysec_9"])

  calls.length = 0
  await run("/sessions")
  const first = pickers.at(-1)!
  expect(first.rows.map((row) => row.id)).not.toContain("arch")
  expect(first.hint).toContain("Ctrl+A shows archived")
  // Ctrl+A commits the toggle at once (no confirm step).
  const toggle = sessionPickerActions.find((action) => action.id === "archived")!
  expect(toggle).toMatchObject({ key: "a", ctrl: true, prompt: "none" })
  const outcome = pickerKey(createPicker({ title: "Sessions", rows: first.rows, actions: sessionPickerActions }), { name: "a", ctrl: true, meta: false, shift: false, sequence: "\x01" })
  expect(outcome.type).toBe("commit")
  await first.onAction!("archived", first.rows[0]!)
  const shown = pickers.at(-1)!
  expect(archivedFlags.at(-1)).toBe(true)
  expect(shown.title).toContain("archived")
  const archivedRow = shown.rows.find((row) => row.id === "arch")!
  expect(archivedRow.tag).toBe("archived")
  await shown.onSelect(archivedRow)
  await shown.onSelect(shown.rows.find((row) => row.id === "new")!)
  expect(calls.filter((call) => call.startsWith("resume") || call.startsWith("open"))).toEqual(["resume arch", "open new"])
})

test("the resumer unarchives before it opens; without an id it offers the directory's sessions incl. archived", async () => {
  const store = createAppStore()
  const patches: string[] = []
  const opened: string[] = []
  const pickers: PickerSpec[] = []
  const resumer = createResumer({
    store,
    directory: "/w",
    client: {
      listSessions: async () => sessions,
      setArchived: async (id: string, archived: boolean) => { patches.push(`${id} ${archived}`); return { ...sessions.find((row) => row.id === id)!, archived } },
    },
    openSession: async (id) => { opened.push(id) },
    openPicker: (picker) => { pickers.push(picker) },
  })
  await resumer.resume("arch")
  expect(patches).toEqual(["arch false"])
  expect(opened).toEqual(["arch"])
  expect(store.state.status).toBe("Resumed Archived work")

  await resumer.resume()
  const picker = pickers.at(-1)!
  expect(picker.title).toContain("Resume")
  expect(picker.rows.map((row) => row.id)).toEqual(["new", "arch", "old"])
  await picker.onSelect(picker.rows[2]!)
  expect(patches).toEqual(["arch false", "old false"])
  expect(opened).toEqual(["arch", "old"])
})

test("Ctrl+D (the eof key) goes to the background in a terminal and only shows the notice in a WebUI tab", () => {
  for (const webTab of [false, true]) {
    const store = createAppStore()
    store.setWebTab(webTab)
    const quits: string[] = []
    toBackground({ store, client: {} as HyaClient, actions: { quit: (mode?: string) => { quits.push(mode ?? "") } } as unknown as AppActions })
    expect(quits).toEqual(webTab ? [] : ["background"])
    if (webTab) expect(store.state.status).toBe(webTabBackgroundNotice)
  }
})
