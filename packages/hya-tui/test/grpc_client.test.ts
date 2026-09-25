import { expect, test } from "bun:test"
import * as grpc from "@grpc/grpc-js"
import * as protoLoader from "@grpc/proto-loader"
import { readdirSync } from "node:fs"
import { resolve } from "node:path"
import { GrpcHyaClient, resolveGrpcOperation } from "../src/grpc_client"
import { argumentsFrom } from "../src/args"

test("maps a frontend request to the v1 gRPC operation and protobuf fields", () => {
  expect(resolveGrpcOperation("GET", "/v1/sessions/hysec_1/events?sinceSeq=12", undefined, "/work")).toEqual({
    service: "Events",
    method: "ListEvents",
    request: { directory: "/work", session: "hysec_1", sinceSeq: "12" },
    streaming: false,
  })
})

test("selects the gRPC listener without treating it as an HTTP URL", () => {
  expect(argumentsFrom(["--grpc", "127.0.0.1:22104", "--dir", "/work"])).toEqual({
    server: "grpc://127.0.0.1:22104", grpc: "127.0.0.1:22104", directory: "/work",
  })
  expect(() => argumentsFrom(["--grpc", "127.0.0.1:22104", "--server", "http://127.0.0.1:22103"])).toThrow()
})

test("connects through native gRPC for bootstrap and live session frames", async () => {
  const protoRoot = resolve(import.meta.dir, "../../../proto")
  const protoDir = resolve(protoRoot, "hya/v1")
  const definitions = protoLoader.loadSync(
    readdirSync(protoDir).filter((name) => name.endsWith(".proto")).map((name) => resolve(protoDir, name)),
    { includeDirs: [protoRoot], keepCase: false, longs: String, enums: String, defaults: false },
  )
  const server = new grpc.Server()
  const requests: Record<string, unknown>[] = []
  server.addService(definitions["hya.v1.Process"] as grpc.ServiceDefinition, {
    GetBootstrap(call: grpc.ServerUnaryCall<Record<string, unknown>, unknown>, callback: grpc.sendUnaryData<unknown>) {
      requests.push(call.request)
      callback(null, { location: { version: "grpc-test" }, agents: [], models: [] })
    },
  })
  server.addService(definitions["hya.v1.Events"] as grpc.ServiceDefinition, {
    StreamSessionEvents(call: grpc.ServerWritableStream<Record<string, unknown>, unknown>) {
      requests.push(call.request)
      call.write({ event: { seq: "12", session: "hysec_1", messageStarted: { message: "msg_1" } } })
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
    const frames: unknown[] = []
    await client.streamSession("hysec_1", "11", (frame) => { frames.push(frame) }, new AbortController().signal)
    expect(frames).toEqual([{ event: { seq: "12", session: "hysec_1", messageStarted: { message: "msg_1" } } }])
    expect(requests).toEqual([{ directory: "/work" }, { session: "hysec_1", sinceSeq: "11" }])
  } finally {
    client.close()
    server.forceShutdown()
  }
})
