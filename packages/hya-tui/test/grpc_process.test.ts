import { expect, test } from "bun:test"
import { mkdtemp, mkdir, rm } from "node:fs/promises"
import { createServer } from "node:net"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { GrpcHyaClient } from "../src/grpc_client"
import type { StreamFrame } from "../src/client"

async function freePort(): Promise<number> {
  const server = createServer()
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done))
  const address = server.address()
  if (!address || typeof address === "string") throw new Error("TCP listener has no port")
  await new Promise<void>((done) => server.close(() => done()))
  return address.port
}

const backendBin = process.env.HYA_BACKEND_BIN

test.skipIf(!backendBin)("OpenTUI gRPC transport drives a real hya backend", async () => {
  const root = resolve(import.meta.dir, "../../..")
  const temp = await mkdtemp(join(tmpdir(), "hya-tui-grpc-"))
  await mkdir(join(temp, "home"))
  const httpPort = await freePort()
  let grpcPort = await freePort()
  while (grpcPort === httpPort) grpcPort = await freePort()
  const processHandle = Bun.spawn([
    backendBin!, "serve", "--bind", `127.0.0.1:${httpPort}`, "--db", join(temp, "sessions.db"),
  ], {
    cwd: root,
    env: {
      ...process.env,
      HOME: join(temp, "home"),
      XDG_CONFIG_HOME: join(temp, "config-home"),
      HYA_GRPC_BIND: `127.0.0.1:${grpcPort}`,
    },
    stdout: "ignore", stderr: "ignore",
  })
  const client = new GrpcHyaClient(`127.0.0.1:${grpcPort}`, root)
  try {
    let connected = false
    for (let attempt = 0; attempt < 80; attempt++) {
      try {
        await client.bootstrap()
        connected = true
        break
      } catch {
        await Bun.sleep(100)
      }
    }
    expect(connected).toBe(true)
    expect((await client.request<{ ok: boolean }>("GET", "/v1/health")).ok).toBe(true)
    expect((await client.listModels()).some((model) => model.id === "hya/offline")).toBe(true)
    expect(await client.listWorkflows()).toBeArray()
    expect(await client.listInteractions()).toEqual([])
    const session = await client.createSession("build", "hya/offline", { workdir: root })
    expect(session.id).toStartWith("hysec_")
    expect((await client.listSessions()).some((row) => row.id === session.id)).toBe(true)
    const turn = await client.createTurn(session.id, "hello over grpc")
    expect(turn.id.length).toBeGreaterThan(0)
    const completed = await client.request<{ state: string }>(
      "POST", `/v1/sessions/${session.id}/turns/${turn.id}/wait?timeoutMs=5000`, {},
    )
    expect(completed.state).not.toBe("TURN_STATE_RUNNING")

    expect((await client.listMessages(session.id)).length).toBeGreaterThan(0)
    const replay = await client.listEvents(session.id, "0")
    expect(replay.events?.length).toBeGreaterThan(0)

    const liveFrames: StreamFrame[] = []
    const liveController = new AbortController()
    const liveTimeout = setTimeout(() => liveController.abort(), 4_000)
    try {
      let subscribed = false
      const streaming = client.streamSession(session.id, replay.nextSeq ?? "0", (frame) => {
        liveFrames.push(frame)
        if (frame.event?.messageFinished) liveController.abort()
      }, liveController.signal, () => { subscribed = true })
      for (let attempt = 0; attempt < 40 && !subscribed; attempt++) await Bun.sleep(25)
      expect(subscribed).toBe(true)
      await client.createTurn(session.id, "second turn over grpc")
      await streaming
    } finally {
      clearTimeout(liveTimeout)
    }
    expect(liveFrames.some((frame) => frame.event?.messageStarted)).toBe(true)
    expect(liveFrames.some((frame) => frame.event?.messageFinished)).toBe(true)
    expect(liveFrames.every((frame) => !frame.event?.seq || Number(frame.event.seq) > Number(replay.nextSeq ?? "0"))).toBe(true)
  } finally {
    client.close()
    processHandle.kill("SIGKILL")
    await processHandle.exited
    await rm(temp, { recursive: true, force: true })
  }
}, 15_000)
