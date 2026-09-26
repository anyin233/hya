import { expect, test } from "bun:test"
import type { MessageInfo, SessionInfo } from "../src/client"
import { createSessionKeeper } from "../src/app/sessionKeeper"

function fakeServer(sessions: Record<string, Partial<SessionInfo>>, messages: Record<string, number> = {}) {
  const deleted: string[] = []
  const reads: string[] = []
  const client = {
    getSession: async (id: string): Promise<SessionInfo> => {
      reads.push(id)
      return { id, agent: "build", workdir: "/w", ...sessions[id] }
    },
    listMessages: async (id: string): Promise<MessageInfo[]> => Array.from({ length: messages[id] ?? 0 }, (_, index) => ({ id: `m${index}`, role: "ROLE_USER" }) as MessageInfo),
    deleteSession: async (id: string): Promise<void> => { deleted.push(id) },
  }
  return { client, deleted, reads }
}

test("an empty session this client created is deleted when it is left", async () => {
  const server = fakeServer({ s1: {} })
  const keeper = createSessionKeeper({ client: server.client })
  keeper.created("s1")
  expect(await keeper.dropIfEmpty("s1")).toBe("deleted")
  expect(server.deleted).toEqual(["s1"])
  // Decided once: leaving it again does nothing.
  expect(await keeper.dropIfEmpty("s1")).toBe("notOurs")
})

test("sessions this client did not create are never touched", async () => {
  const server = fakeServer({ other: {} })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.dropIfEmpty("other")).toBe("notOurs")
  expect(server.reads).toEqual([])
  expect(server.deleted).toEqual([])
})

test("a session with messages, a running turn, a title, or a parent is kept (re-checked on the server)", async () => {
  const server = fakeServer({ used: {}, busy: { busy: true }, titled: { title: "Plan" }, child: { parent: "p" } }, { used: 2 })
  const keeper = createSessionKeeper({ client: server.client })
  for (const id of ["used", "busy", "titled", "child"]) {
    keeper.created(id)
    expect(await keeper.dropIfEmpty(id)).toBe("kept")
  }
  expect(server.deleted).toEqual([])
})

test("a session the user sent something in is kept without asking the server", async () => {
  const server = fakeServer({ s1: {} })
  const keeper = createSessionKeeper({ client: server.client, localBusy: (id) => id === "queued" })
  keeper.created("s1")
  keeper.used("s1")
  expect(await keeper.dropIfEmpty("s1")).toBe("notOurs")
  keeper.created("queued")
  expect(await keeper.dropIfEmpty("queued")).toBe("kept")
  expect(server.reads).toEqual([])
})

test("a read failure keeps the session (never delete on doubt)", async () => {
  const keeper = createSessionKeeper({
    client: {
      getSession: async () => { throw new Error("offline") },
      listMessages: async () => [],
      deleteSession: async () => { throw new Error("must not delete") },
    },
  })
  keeper.created("s1")
  expect(await keeper.dropIfEmpty("s1")).toBe("kept")
})

/** A fake server with parents and an archive call, for the exit modes. */
function exitServer(sessions: Record<string, Partial<SessionInfo>>, messages: Record<string, number> = {}, archiveFails = false) {
  const base = fakeServer(sessions, messages)
  const archived: string[] = []
  const client = {
    ...base.client,
    archiveSession: async (id: string): Promise<void> => {
      if (archiveFails) throw new Error("offline")
      archived.push(id)
    },
  }
  return { ...base, client, archived }
}

test("a graceful exit archives the session; background and signal exits leave it running", async () => {
  const server = exitServer({ s1: {} }, { s1: 3 })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.leave("s1", "background")).toBe("kept")
  expect(await keeper.leave("s1", "signal")).toBe("kept")
  expect(server.archived).toEqual([])
  expect(await keeper.leave("s1", "archive")).toBe("archived")
  expect(server.archived).toEqual(["s1"])
  expect(server.deleted).toEqual([])
})

test("a graceful exit from a subagent's view archives its root session", async () => {
  const server = exitServer({ root: {}, child: { parent: "mid" }, mid: { parent: "root" } })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.leave("child", "archive")).toBe("archived")
  expect(server.archived).toEqual(["root"])
})

test("an empty session this client created is dropped on every exit, never archived", async () => {
  for (const mode of ["archive", "background", "signal"] as const) {
    const server = exitServer({ s1: {} })
    const keeper = createSessionKeeper({ client: server.client })
    keeper.created("s1")
    expect(await keeper.leave("s1", mode)).toBe("deleted")
    expect(server.deleted).toEqual(["s1"])
    expect(server.archived).toEqual([])
  }
})

test("a running turn is archived as is (archiving never cancels it); an archive failure keeps the session", async () => {
  const busy = exitServer({ s1: { busy: true } })
  const keeper = createSessionKeeper({ client: busy.client, localBusy: () => true })
  keeper.created("s1")
  expect(await keeper.leave("s1", "archive")).toBe("archived")
  expect(busy.archived).toEqual(["s1"])
  const failing = exitServer({ s1: {} }, { s1: 1 }, true)
  expect(await createSessionKeeper({ client: failing.client }).leave("s1", "archive")).toBe("kept")
})
