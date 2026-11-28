import { expect, test } from "bun:test"
import * as grpc from "@grpc/grpc-js"
import * as protoLoader from "@grpc/proto-loader"
import { readFileSync, readdirSync } from "node:fs"
import { resolve } from "node:path"
import { GrpcHyaClient, resolveGrpcOperation } from "../src/grpc_client"
import { parseArguments } from "../src/cli"
import catalog from "../src/operations.json"
import type { MessagePart } from "../src/client"
import { TranscriptOverlay } from "../src/state/overlay"

test("maps a frontend request to the v1 gRPC operation and protobuf fields", () => {
  expect(resolveGrpcOperation("GET", "/v2/sessions/hysec_1/events?sinceSeq=12", undefined, "/work")).toEqual({
    service: "Events",
    method: "ListEvents",
    request: { directory: "/work", session: "hysec_1", sinceSeq: "12" },
    streaming: false,
  })
  expect(resolveGrpcOperation("GET", "/v2/bundles/example/api/sub/route", undefined, "/work")).toMatchObject({
    service: "BundleApi", method: "InvokeGlobalBundleApi", request: { bundle: "example", path: "sub/route" },
  })
})

test("selects the gRPC listener without treating it as an HTTP URL", () => {
  expect(parseArguments(["--grpc", "127.0.0.1:22104", "--dir", "/work"])).toEqual({
    grpc: "127.0.0.1:22104", directory: "/work", continue: false,
  })
  expect(() => parseArguments(["--grpc", "127.0.0.1:22104", "--server", "http://127.0.0.1:22103"])).toThrow()
})

test("packaged protobuf filenames and bytes match the server contract (run cargo run -p xtask -- gen-api)", () => {
  const bundled = resolve(import.meta.dir, "../src/proto/hya/v2")
  const contract = resolve(import.meta.dir, "../../../proto/hya/v2")
  const protoFiles = (dir: string) => readdirSync(dir).filter(name => name.endsWith(".proto")).sort()
  const files = protoFiles(contract)
  expect(protoFiles(bundled)).toEqual(files)
  const mismatches = files.filter(name => !readFileSync(resolve(bundled, name)).equals(readFileSync(resolve(contract, name))))
  expect(mismatches).toEqual([])
})

test("every API command maps to a method in the packaged gRPC contract", () => {
  const protoRoot = resolve(import.meta.dir, "../src/proto")
  const protoDir = resolve(protoRoot, "hya/v2")
  const definitions = protoLoader.loadSync(
    readdirSync(protoDir).filter((name) => name.endsWith(".proto")).map((name) => resolve(protoDir, name)),
    { includeDirs: [protoRoot], keepCase: false, longs: String, enums: String, defaults: false },
  )
  for (const row of catalog) {
    const path = row.path.replace(/\{[^}]+\}/g, "example")
    const call = resolveGrpcOperation(row.method, path, undefined, "/work")
    const service = definitions[`hya.v2.${call.service}`] as Record<string, unknown> | undefined
    expect(service?.[call.method], `${row.method} ${row.path} → ${call.service}.${call.method}`).toBeDefined()
    expect(call.streaming).toBe(row.streaming)
  }
})

test("native gRPC bootstraps and applies a streamed part deletion before message finish", async () => {
  const protoRoot = resolve(import.meta.dir, "../../../proto")
  const protoDir = resolve(protoRoot, "hya/v2")
  const definitions = protoLoader.loadSync(
    readdirSync(protoDir).filter((name) => name.endsWith(".proto")).map((name) => resolve(protoDir, name)),
    { includeDirs: [protoRoot], keepCase: false, longs: String, enums: String, defaults: false },
  )
  const message = "msg_1"
  const part = "part_1"
  const session = "hysec_1"
  const frames = [
    { event: { session, seq: "12", messageStarted: { message, role: "ROLE_ASSISTANT" } } },
    { event: { session, partStarted: { message, part, kind: "text" } } },
    { event: { session, partAppended: { message, part, textDelta: "partial reply" } } },
    { event: { session, seq: "13", partDeleted: { message, part } } },
  ]
  const server = new grpc.Server()
  const requests: Record<string, unknown>[] = []
  server.addService(definitions["hya.v2.Process"] as grpc.ServiceDefinition, {
    GetBootstrap(call: grpc.ServerUnaryCall<Record<string, unknown>, unknown>, callback: grpc.sendUnaryData<unknown>) {
      requests.push(call.request)
      callback(null, { location: { version: "grpc-test" }, agents: [], models: [] })
    },
  })
  server.addService(definitions["hya.v2.Events"] as grpc.ServiceDefinition, {
    StreamSessionEvents(call: grpc.ServerWritableStream<Record<string, unknown>, unknown>) {
      requests.push(call.request)
      for (const frame of frames) call.write(frame)
      call.end()
    },
    StreamGlobalEvents(call: grpc.ServerWritableStream<Record<string, unknown>, unknown>) {
      requests.push(call.request)
      call.write({ event: { session: "hysec_2", catalogUpdated: {} } })
      call.end()
    },
  })
  const port = await new Promise<number>((done, fail) => {
    server.bindAsync("127.0.0.1:0", grpc.ServerCredentials.createInsecure(), (error, boundPort) => {
      if (error) fail(error)
      else done(boundPort)
    })
  })
  const client = new GrpcHyaClient(`127.0.0.1:${port}`, "/work")
  try {
    expect((await client.bootstrap()).location?.version).toBe("grpc-test")
    const overlay = new TranscriptOverlay("11")
    const snapshots: (MessagePart[] | undefined)[] = []
    let opened = false
    await client.streamSession(session, "11", ({ event }) => {
      if (event) {
        overlay.apply(event)
        snapshots.push(overlay.messages()[0]?.parts)
      }
    }, new AbortController().signal, () => { opened = true })
    expect(opened).toBe(true)
    expect(snapshots.at(-2)).toEqual([{ id: part, text: { text: "partial reply" } }])
    expect(snapshots.at(-1)).toEqual([])
    expect(overlay.lastSeq).toBe("13")
    expect(overlay.messages()[0]?.finish).toBeUndefined()
    const global: unknown[] = []
    await client.streamGlobal((frame) => { global.push(frame) }, new AbortController().signal)
    expect(global).toEqual([{ event: { session: "hysec_2", catalogUpdated: {} } }])
    expect(requests).toEqual([{ directory: "/work" }, { session: "hysec_1", sinceSeq: "11" }, { interactionsOnly: true }])
  } finally {
    client.close()
    server.forceShutdown()
  }
})

for (const errors of [[], [{ bundleId: "acme/tools", message: "bad config" }]]) {
  test(`refreshBundles uses Catalog.RefreshBundles over gRPC (${errors.length})`, async () => {
    const calls: unknown[] = []
    const client = new GrpcHyaClient("backend:8080", "/work", {
      unary: async (service, method, request) => { calls.push([service, method, request]); return { generation: "2", errors } },
      stream: async () => {},
      close: () => {},
    })
    try {
      if (errors.length) await expect(client.refreshBundles()).rejects.toThrow("acme/tools: bad config")
      else await client.refreshBundles()
      expect(calls).toEqual([["Catalog", "RefreshBundles", { directory: "/work" }]])
    } finally { client.close() }
  })
}
