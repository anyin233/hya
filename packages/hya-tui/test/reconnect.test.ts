import { expect, test } from "bun:test"
import { createReconnector, switchNotice, type ServerSwitch } from "../src/app/reconnect"

/** A reconnector over fakes: `answers` are the probe results in order (then `false`). */
function harness(answers: boolean[], next: () => Promise<ServerSwitch>) {
  const probes: string[] = []
  const switched: ServerSwitch[] = []
  const statuses: string[] = []
  let reconnects = 0
  let url = "http://127.0.0.1:1"
  const reconnector = createReconnector({
    url: () => url,
    probe: async (target) => { probes.push(target); return answers.shift() ?? false },
    reconnect: async () => { reconnects++; return next() },
    switchTo: async (target) => { switched.push(target); url = target.url },
    status: (text) => statuses.push(text),
    sleep: async () => {},
  })
  return { reconnector, probes, switched, statuses, reconnects: () => reconnects }
}

const moved: ServerSwitch = { url: "http://127.0.0.1:2", pid: 22, started: false }
const started: ServerSwitch = { url: "http://127.0.0.1:3", pid: 33, started: true }

test("a server that still answers is a blip: no rediscovery", async () => {
  const h = harness([true], async () => moved)
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.switched).toEqual([])
  // One failed probe and then an answer is a blip too.
  const again = harness([false, true], async () => moved)
  await again.reconnector.lost()
  expect(again.probes).toEqual(["http://127.0.0.1:1", "http://127.0.0.1:1"])
  expect(again.reconnects()).toBe(0)
})

test("a server that fails two probes is gone: find or start the next one, switch, and say so", async () => {
  const h = harness([false, false], async () => moved)
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(1)
  expect(h.switched).toEqual([moved])
  expect(h.statuses.at(-1)).toBe("Server moved · now pid 22")
  const s = harness([false, false], async () => started)
  await s.reconnector.lost()
  expect(s.statuses.at(-1)).toBe("Started a new server · pid 33")
  expect(s.statuses).toContain("Server stopped · reconnecting…")
})

test("concurrent losses (both streams fail) run one rediscovery", async () => {
  let release!: () => void
  const gate = new Promise<void>((resolve) => (release = resolve))
  const h = harness([false, false], async () => { await gate; return moved })
  const first = h.reconnector.lost()
  const second = h.reconnector.lost()
  expect(h.reconnector.busy()).toBe(true)
  release()
  await Promise.all([first, second])
  expect(h.reconnects()).toBe(1)
  expect(h.switched).toHaveLength(1)
  expect(h.reconnector.busy()).toBe(false)
})

test("a failed rediscovery is reported, and the next loss tries again", async () => {
  let fail = true
  const h = harness([false, false, false, false], async () => {
    if (fail) throw new Error("the hya server daemon did not answer within 60 s")
    return started
  })
  await h.reconnector.lost()
  expect(h.statuses.at(-1)).toBe("Server lost: the hya server daemon did not answer within 60 s · retrying")
  expect(h.switched).toEqual([])
  fail = false
  await h.reconnector.lost()
  expect(h.switched).toEqual([started])
})

test("rediscovering the same server (slow, not gone) switches nothing", async () => {
  const h = harness([false, false], async () => ({ url: "http://127.0.0.1:1", pid: 11, started: false }))
  await h.reconnector.lost()
  expect(h.switched).toEqual([])
})

test("notices", () => {
  expect(switchNotice(moved)).toBe("Server moved · now pid 22")
  expect(switchNotice(started)).toBe("Started a new server · pid 33")
})
