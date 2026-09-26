import { expect, test } from "bun:test"
import type { SessionInfo } from "../src/client"
import { createSessionKeeper } from "../src/app/sessionKeeper"

/** A fake server with parents and an archive call, for the exit modes. */
function exitServer(sessions: Record<string, Partial<SessionInfo>>, archiveFails = false) {
  const reads: string[] = []
  const archived: string[] = []
  const client = {
    getSession: async (id: string): Promise<SessionInfo> => {
      reads.push(id)
      return { id, agent: "build", workdir: "/w", ...sessions[id] }
    },
    archiveSession: async (id: string): Promise<void> => {
      if (archiveFails) throw new Error("offline")
      archived.push(id)
    },
  }
  return { client, reads, archived }
}

test("a graceful exit archives the session; background and signal exits leave it running", async () => {
  const server = exitServer({ s1: {} })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.leave("s1", "background")).toBe("kept")
  expect(await keeper.leave("s1", "signal")).toBe("kept")
  expect(server.reads).toEqual([])
  expect(server.archived).toEqual([])
  expect(await keeper.leave("s1", "archive")).toBe("archived")
  expect(server.archived).toEqual(["s1"])
})

test("a graceful exit from a subagent's view archives its root session", async () => {
  const server = exitServer({ root: {}, child: { parent: "mid" }, mid: { parent: "root" } })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.leave("child", "archive")).toBe("archived")
  expect(server.archived).toEqual(["root"])
})

test("an unused session this client created is left to the daemon on every exit: no request, no wait", async () => {
  for (const mode of ["archive", "background", "signal"] as const) {
    const server = exitServer({ s1: { ephemeral: true } })
    const keeper = createSessionKeeper({ client: server.client })
    keeper.created("s1")
    expect(keeper.isFresh("s1")).toBe(true)
    expect(await keeper.leave("s1", mode)).toBe("unused")
    // The daemon drops it once no client watches it: nothing to send, nothing to wait for.
    expect(server.reads).toEqual([])
    expect(server.archived).toEqual([])
  }
})

test("once the user sent something the session is archived on a graceful exit", async () => {
  const server = exitServer({ s1: {} })
  const keeper = createSessionKeeper({ client: server.client })
  keeper.created("s1")
  keeper.used("s1")
  expect(keeper.isFresh("s1")).toBe(false)
  expect(await keeper.leave("s1", "archive")).toBe("archived")
  expect(server.archived).toEqual(["s1"])
})

test("another client's unused session opened here is not archived (archiving would keep it)", async () => {
  const server = exitServer({ theirs: { ephemeral: true } })
  const keeper = createSessionKeeper({ client: server.client })
  expect(await keeper.leave("theirs", "archive")).toBe("unused")
  expect(server.archived).toEqual([])
})

test("an archive failure keeps the session as is", async () => {
  const failing = exitServer({ s1: {} }, true)
  expect(await createSessionKeeper({ client: failing.client }).leave("s1", "archive")).toBe("kept")
})
