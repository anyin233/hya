import { expect, test } from "bun:test"
import { secretMask } from "../src/state/format"
import { bridgeArgv, bridgeEnv, containsRelayLink, historyEntry, looksLikeRelayLink, parseBridgeReady, parseConnectRemote, redactRelayLinks, startBridge, takeServerToken, type BridgeProcess } from "../src/bridge"

const link = "hya+insecure://127.0.0.1:8766/room123?t=grpc#SECRETKEY.SECRETPSK"
const readyLine = JSON.stringify({ url: "http://127.0.0.1:40001", room: "room123", proxy: "hya+insecure://127.0.0.1:8766", label: "remote: 127.0.0.1:8766/room123" })

/** A scripted bridge child: the test pushes stdout/stderr lines and decides when it exits. */
export function fakeBridge() {
  const enc = new TextEncoder()
  let out!: ReadableStreamDefaultController<Uint8Array>
  let err!: ReadableStreamDefaultController<Uint8Array>
  let exit!: (code: number) => void
  const stdin: string[] = []
  const signals: string[] = []
  let ended = false
  let closed = false
  const exited = new Promise<number>((resolve) => (exit = resolve))
  const finish = (code: number) => {
    if (closed) return
    closed = true
    try { out.close() } catch { /* closed */ }
    try { err.close() } catch { /* closed */ }
    exit(code)
  }
  const process: BridgeProcess = {
    pid: 4242,
    write: (text) => { stdin.push(text) },
    end: () => { ended = true },
    stdout: new ReadableStream({ start: (controller) => { out = controller } }),
    stderr: new ReadableStream({ start: (controller) => { err = controller } }),
    exited,
    kill: (signal = "SIGTERM") => { signals.push(signal); finish(0) },
  }
  return {
    process,
    stdin,
    signals,
    get ended() { return ended },
    stdout: (line: string) => out.enqueue(enc.encode(`${line}\n`)),
    stderr: (line: string) => err.enqueue(enc.encode(`${line}\n`)),
    exit: finish,
  }
}

test("bridgeArgv never carries the link and passes --transport / --relay-ca through", () => {
  expect(bridgeArgv("/bin/hya")).toEqual(["/bin/hya", "bridge", "-", "--json", "--exit-with-stdin"])
  expect(bridgeArgv("/bin/hya", { transport: "ws", relayCa: "/ca.pem" })).toEqual(["/bin/hya", "bridge", "-", "--json", "--exit-with-stdin", "--transport", "ws", "--relay-ca", "/ca.pem"])
})

test("parseConnectRemote: link, flags, and errors that never echo the link", () => {
  expect(parseConnectRemote([link])).toEqual({ link, flags: {} })
  expect(parseConnectRemote(["--transport", "grpc", link, "--relay-ca", "/ca.pem"])).toEqual({ link, flags: { transport: "grpc", relayCa: "/ca.pem" } })
  expect(parseConnectRemote([])).toEqual({ flags: {} })
  expect(() => parseConnectRemote(["--transport", "carrier-pigeon"])).toThrow("--transport takes auto, grpc, or ws")
  try {
    parseConnectRemote([link, link])
    throw new Error("expected a usage error")
  } catch (error) {
    expect(String(error)).toContain("Usage: /connect-remote")
    expect(String(error)).not.toContain("SECRET")
  }
})

test("relay links are recognised, redacted, and dropped from history entries", () => {
  expect(looksLikeRelayLink(link)).toBe(true)
  expect(looksLikeRelayLink("hya://relay.example.com/room")).toBe(false)
  expect(looksLikeRelayLink("https://example.com/#x")).toBe(false)
  expect(containsRelayLink(`please use ${link} now`)).toBe(true)
  expect(containsRelayLink("hya://relay.example.com/room")).toBe(false)
  expect(redactRelayLinks(`relay ${link} failed`)).toBe("relay hya+insecure://127.0.0.1:8766/room123?t=grpc#… failed")
  expect(historyEntry(`/connect-remote --transport ws ${link}`)).toBe("/connect-remote --transport ws")
  expect(historyEntry(`/connect-remote ${link}`)).toBe("/connect-remote")
  expect(historyEntry("/models")).toBe("/models")
})

test("parseBridgeReady reads the JSON readiness line", () => {
  expect(parseBridgeReady(readyLine)).toEqual({ url: "http://127.0.0.1:40001", room: "room123", proxy: "hya+insecure://127.0.0.1:8766", label: "remote: 127.0.0.1:8766/room123" })
  expect(parseBridgeReady("hya bridge listening on http://127.0.0.1:1")).toBeUndefined()
})

test("startBridge writes the link to stdin, keeps it open, and resolves on the readiness line", async () => {
  const fake = fakeBridge()
  const argvs: string[][] = []
  const lines: string[] = []
  const starting = startBridge({ bin: "/bin/hya", link, spawn: (argv) => { argvs.push(argv); return fake.process }, onLine: (line) => lines.push(line) })
  fake.stderr("hya bridge: relay hya+insecure://127.0.0.1:8766: grpc binding")
  fake.stdout(readyLine)
  const bridge = await starting
  expect(argvs).toEqual([["/bin/hya", "bridge", "-", "--json", "--exit-with-stdin"]])
  expect(argvs.flat().join(" ")).not.toContain("SECRET")
  expect(fake.stdin).toEqual([`${link}\n`])
  expect(fake.ended).toBe(false)
  expect(bridge.url).toBe("http://127.0.0.1:40001")
  expect(bridge.label).toBe("remote: 127.0.0.1:8766/room123")
  expect(JSON.stringify(bridge)).not.toContain("SECRET")
  await Bun.sleep(0)
  expect(lines).toEqual(["hya bridge: relay hya+insecure://127.0.0.1:8766: grpc binding"])

  // stop(): close stdin first; the child exits 0 by itself.
  const stopping = bridge.stop(1_000)
  expect(fake.ended).toBe(true)
  expect(bridge.stopping).toBe(true)
  fake.exit(0)
  await stopping
  expect(fake.signals).toEqual([])
})

test("stop() sends SIGTERM when the child is still alive after the grace period", async () => {
  const fake = fakeBridge()
  const starting = startBridge({ bin: "hya", link, spawn: () => fake.process })
  fake.stdout(readyLine)
  const bridge = await starting
  await bridge.stop(20)
  expect(fake.signals).toEqual(["SIGTERM"])
})

test("a failing bridge rejects with its one-line reason, without the link", async () => {
  const fake = fakeBridge()
  const starting = startBridge({ bin: "hya", link, spawn: () => fake.process })
  fake.stderr("hya bridge: the remote backend rejected the relay link hya+insecure://127.0.0.1:8766/room123 (rotated or wrong link); ask for a new one")
  fake.exit(1)
  const error = await starting.then(() => undefined, (reason: unknown) => reason)
  expect(String(error)).toContain("the remote backend rejected the relay link")
  expect(String(error)).not.toContain("hya bridge:")
  expect(String(error)).not.toContain("SECRET")
})

test("an `Error:` line (the CLI's fatal error form) is the reason too", async () => {
  const fake = fakeBridge()
  const starting = startBridge({ bin: "hya", link, spawn: () => fake.process })
  fake.stderr("Error: the remote backend rejected the relay link hya+insecure://127.0.0.1:8766/room123 (rotated or wrong link); ask for a new one")
  fake.exit(1)
  const error = await starting.then(() => undefined, (reason: unknown) => reason) as Error
  expect(error.message).toStartWith("the remote backend rejected the relay link")
})

test("a bridge that never gets ready times out and is stopped", async () => {
  const fake = fakeBridge()
  const error = await startBridge({ bin: "hya", link, spawn: () => fake.process, timeoutMs: 30 }).then(() => undefined, (reason: unknown) => reason)
  expect(String(error)).toContain("no answer from the relay within")
  expect(fake.ended).toBe(true)
  expect(fake.signals).toEqual(["SIGTERM"])
})

test("startBridge refuses input that is not a relay link without spawning", async () => {
  let spawned = false
  const error = await startBridge({ bin: "hya", link: "not-a-link", spawn: () => { spawned = true; return fakeBridge().process } }).then(() => undefined, (reason: unknown) => reason)
  expect(String(error)).toContain("not a relay link")
  expect(spawned).toBe(false)
})

test("secretMask shows at most 32 bullets and the character count", () => {
  expect(secretMask(0)).toBe(" ")
  expect(secretMask(1)).toBe("•  1 character")
  expect(secretMask(40)).toBe(`${"•".repeat(32)}…  40 characters`)
})

test("parseBridgeReady reads the bridge token; a missing or unprintable one is left out", () => {
  const token = "0123456789abcdef".repeat(4)
  const base = { url: "http://127.0.0.1:40001", room: "room123", proxy: "hya+insecure://127.0.0.1:8766", label: "remote: 127.0.0.1:8766/room123" }
  expect(parseBridgeReady(JSON.stringify({ ...base, token }))).toEqual({ ...base, token })
  expect(parseBridgeReady(JSON.stringify(base))).toEqual(base)
  for (const bad of ["", "has space", "tab\there", "esc\x1b[31m", "ünï", 42, null]) {
    expect(parseBridgeReady(JSON.stringify({ ...base, token: bad }))).toEqual(base)
  }
})

test("parseBridgeReady strips terminal controls from the shown fields", () => {
  const ready = parseBridgeReady(JSON.stringify({ url: "http://127.0.0.1:40001", room: "r\x1b]0;x\x07oom", proxy: "hya://relay\x1b[2J", label: "remote: \x1b[31mrelay/room\x1b[0m\x9b" }))
  expect(ready).toEqual({ url: "http://127.0.0.1:40001", room: "room", proxy: "hya://relay", label: "remote: relay/room" })
})

test("bridgeEnv drops HYA_RELAY_LINK and HYA_SERVER_TOKEN and keeps the rest", () => {
  const env = { PATH: "/bin", HOME: "/h", HYA_RELAY_LINK: link, HYA_SERVER_TOKEN: "t".repeat(64), EMPTY: undefined }
  expect(bridgeEnv(env)).toEqual({ PATH: "/bin", HOME: "/h" })
  // The input is not changed.
  expect(env.HYA_RELAY_LINK).toBe(link)
})

test("startBridge hands on the readiness token", async () => {
  const fake = fakeBridge()
  const token = "f".repeat(64)
  const starting = startBridge({ bin: "hya", link, spawn: () => fake.process })
  fake.stdout(JSON.stringify({ ...JSON.parse(readyLine), token }))
  const bridge = await starting
  expect(bridge.token).toBe(token)
  fake.exit(0)
})

test("bridge stderr lines lose terminal controls before onLine, lastLine, and the failure reason", async () => {
  const fake = fakeBridge()
  const lines: string[] = []
  const starting = startBridge({ bin: "hya", link, spawn: () => fake.process, onLine: (line) => lines.push(line) })
  fake.stderr("hya bridge: \x1b]0;pwned\x07relay \x1b[31moffline\x1b[0m\r")
  fake.stderr("hya bridge: \x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\ \x9b2Jgone\x7f")
  fake.exit(1)
  const error = await starting.then(() => undefined, (reason: unknown) => reason) as Error
  expect(lines).toEqual(["hya bridge: relay offline", "hya bridge: click gone"])
  expect(error.message).toBe("click gone")

  // And lastLine() of a running bridge.
  const next = fakeBridge()
  const running = startBridge({ bin: "hya", link, spawn: () => next.process })
  next.stdout(readyLine)
  const bridge = await running
  next.stderr("hya bridge: remote \x1b[1moffline\x1b[0m\x07")
  await Bun.sleep(0)
  expect(bridge.lastLine()).toBe("hya bridge: remote offline")
  next.exit(0)
})

test("takeServerToken reads HYA_SERVER_TOKEN once and removes it and HYA_RELAY_LINK", () => {
  const env: Record<string, string | undefined> = { PATH: "/bin", HYA_SERVER_TOKEN: "e".repeat(64), HYA_RELAY_LINK: link }
  expect(takeServerToken(env)).toBe("e".repeat(64))
  expect(env).toEqual({ PATH: "/bin" })
  expect(takeServerToken(env)).toBeUndefined()
  expect(takeServerToken({ HYA_SERVER_TOKEN: "" })).toBeUndefined()
})
