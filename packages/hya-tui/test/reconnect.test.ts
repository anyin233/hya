import { expect, test } from "bun:test"
import { createReconnector, stoppedNotice, switchNotice, type ServerSwitch } from "../src/app/reconnect"

/**
 * A reconnector over fakes: `answers` are the probe results in order (then
 * `false`); `found` answers the find-only lookups in order (then nothing).
 */
function harness(answers: boolean[], next: () => Promise<ServerSwitch>, found: (ServerSwitch | undefined)[] = []) {
  const probes: string[] = []
  const switched: ServerSwitch[] = []
  const statuses: string[] = []
  const stoppedStates: boolean[] = []
  let reconnects = 0
  let finds = 0
  let clock = 0
  let url = "http://127.0.0.1:1"
  const reconnector = createReconnector({
    url: () => url,
    probe: async (target) => { probes.push(target); return answers.shift() ?? false },
    reconnect: async () => { reconnects++; return next() },
    find: async () => { finds++; return found.shift() },
    switchTo: async (target) => { switched.push(target); url = target.url },
    status: (text) => statuses.push(text),
    onStopped: (stopped) => stoppedStates.push(stopped),
    sleep: async (ms) => { clock += ms },
    now: () => clock,
  })
  return { reconnector, probes, switched, statuses, stoppedStates, reconnects: () => reconnects, finds: () => finds, clock: () => clock }
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

test("after `serverStopping {stop}` the TUI does not start a server: it stays stopped and says how to start one", async () => {
  const h = harness([false, false], async () => started)
  h.reconnector.stopping("http://127.0.0.1:1/", "stop")
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.switched).toEqual([])
  expect(h.reconnector.stopped()).toBe(true)
  expect(h.stoppedStates).toEqual([true])
  expect(h.statuses.at(-1)).toBe("Backend stopped (hya serve stop) · /reconnect starts it again")
  // The streams keep retrying: later losses never start one, and never probe the dead URL again.
  const probes = h.probes.length
  await h.reconnector.lost()
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.probes.length).toBe(probes)
  expect(h.statuses.at(-1)).toBe("Backend stopped (hya serve stop) · /reconnect starts it again")
})

test("a plain signal (and an unknown reason) is treated like stop", async () => {
  const h = harness([false, false], async () => started)
  h.reconnector.stopping("http://127.0.0.1:1", "signal")
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.statuses.at(-1)).toBe("Backend stopped (signal) · /reconnect starts it again")
  const u = harness([false, false], async () => started)
  u.reconnector.stopping("http://127.0.0.1:1", "maintenance")
  await u.reconnector.lost()
  expect(u.reconnects()).toBe(0)
  expect(u.reconnector.stopped()).toBe(true)
})

test("a stopped TUI attaches to a server another client started, but never starts one", async () => {
  const h = harness([false, false], async () => started, [undefined, moved])
  h.reconnector.stopping("http://127.0.0.1:1", "stop")
  await h.reconnector.lost()
  await h.reconnector.lost()
  expect(h.switched).toEqual([])
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.switched).toEqual([moved])
  expect(h.reconnector.stopped()).toBe(false)
  expect(h.stoppedStates).toEqual([true, false])
  expect(h.statuses.at(-1)).toBe("Server moved · now pid 22")
})

test("/reconnect after a stop finds or starts a server now", async () => {
  const h = harness([false, false], async () => started)
  h.reconnector.stopping("http://127.0.0.1:1", "stop")
  await h.reconnector.lost()
  await h.reconnector.reconnectNow()
  expect(h.reconnects()).toBe(1)
  expect(h.switched).toEqual([started])
  expect(h.reconnector.stopped()).toBe(false)
  expect(h.statuses.at(-1)).toBe("Started a new server · pid 33")
  // After the switch an unexpected loss auto-starts again (the stop is forgotten).
  const again = harness([false, false, false, false], async () => started)
  again.reconnector.stopping("http://127.0.0.1:1", "stop")
  await again.reconnector.lost()
  await again.reconnector.reconnectNow()
  await again.reconnector.lost()
  expect(again.reconnects()).toBe(2)
})

test("a failed /reconnect is reported and the TUI stays stopped", async () => {
  const h = harness([false, false], async () => { throw new Error("hya binary not found") })
  h.reconnector.stopping("http://127.0.0.1:1", "stop")
  await h.reconnector.lost()
  await h.reconnector.reconnectNow()
  expect(h.reconnector.stopped()).toBe(true)
  expect(h.statuses.at(-1)).toBe("Reconnect failed: hya binary not found · /reconnect to try again")
})

test("/reconnect while connected to a live server just says so", async () => {
  const h = harness([], async () => ({ url: "http://127.0.0.1:1", pid: 11, started: false }))
  await h.reconnector.reconnectNow()
  expect(h.switched).toEqual([])
  expect(h.statuses.at(-1)).toBe("Connected · pid 11")
})

test("after `serverStopping {restart}` the TUI waits for the next server and attaches, never starting one", async () => {
  const h = harness([false, false], async () => started, [undefined, undefined, undefined, moved])
  h.reconnector.stopping("http://127.0.0.1:1", "restart")
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.finds()).toBe(4)
  expect(h.switched).toEqual([moved])
  expect(h.statuses).toContain("Backend restarting (hya serve restart) · waiting for the new one…")
  expect(h.statuses.at(-1)).toBe("Server moved · now pid 22")
  expect(h.reconnector.stopped()).toBe(false)
})

test("a restart that never brings a server back ends stopped after about 60 s", async () => {
  const h = harness([false, false], async () => started)
  h.reconnector.stopping("http://127.0.0.1:1", "restart")
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(0)
  expect(h.clock()).toBeGreaterThanOrEqual(60_000)
  expect(h.clock()).toBeLessThan(62_000)
  expect(h.reconnector.stopped()).toBe(true)
  expect(h.statuses.at(-1)).toBe("Backend did not come back after hya serve restart · /reconnect starts it again")
})

test("a reason told by another server does not apply to this one", async () => {
  const h = harness([false, false], async () => started)
  h.reconnector.stopping("http://127.0.0.1:9", "stop")
  await h.reconnector.lost()
  expect(h.reconnects()).toBe(1)
  expect(h.switched).toEqual([started])
})

test("stopped notices", () => {
  expect(stoppedNotice("stop")).toBe("Backend stopped (hya serve stop) · /reconnect starts it again")
  expect(stoppedNotice("signal")).toBe("Backend stopped (signal) · /reconnect starts it again")
})
