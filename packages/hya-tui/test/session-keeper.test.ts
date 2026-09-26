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
