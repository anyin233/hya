/** Native hya.v1 gRPC transport for the shared OpenTUI workflows. */
import * as grpc from "@grpc/grpc-js"
import * as protoLoader from "@grpc/proto-loader"
import { readdirSync } from "node:fs"
import { resolve } from "node:path"
import openapi from "../../../docs/protocol/openapi.json"
import { HttpError, HyaClient, type StreamFrame } from "./client"

export interface GrpcOperation {
  service: string
  method: string
  request: Record<string, unknown>
  streaming: boolean
}

interface OperationRow {
  httpMethod: string
  path: string
  pattern: RegExp
  placeholders: string[]
  service: string
  rpcMethod: string
  streaming: boolean
}

function camel(name: string): string {
  return name.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase())
}

function operationRows(): OperationRow[] {
  const paths = openapi.paths as Record<string, Record<string, { operationId?: string; "x-server-streaming"?: boolean }>>
  return Object.entries(paths).flatMap(([path, methods]) => Object.entries(methods).map(([httpMethod, detail]) => {
    const placeholders: string[] = []
    const parts = path.split("/").map((part) => {
      if (part.startsWith("{") && part.endsWith("}")) {
        placeholders.push(camel(part.slice(1, -1)))
        return "([^/]+)"
      }
      return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    })
    const [service = "", rpcMethod = ""] = (detail.operationId ?? "").split(".")
    return {
      httpMethod: httpMethod.toUpperCase(), path,
      pattern: new RegExp(`^${parts.join("/")}$`), placeholders,
      service, rpcMethod, streaming: detail["x-server-streaming"] === true,
    }
  }))
}

const operations = operationRows()

/** Translate a generated HTTP binding into its exact gRPC service and request. */
export function resolveGrpcOperation(method: string, path: string, body: unknown, directory: string): GrpcOperation {
  if (!path.startsWith("/v1/") || path.startsWith("//")) throw new Error("API path must start with /v1/")
  const url = new URL(path, "http://hya.invalid")
  const match = operations
    .filter((row) => row.httpMethod === method.toUpperCase())
    .map((row) => ({ row, segments: row.pattern.exec(url.pathname) }))
    .find(({ segments }) => segments !== null)
  if (!match || !match.segments || !match.row.service || !match.row.rpcMethod) {
    throw new Error(`No hya.v1 gRPC operation for ${method.toUpperCase()} ${url.pathname}`)
  }
  if (body !== undefined && (body === null || typeof body !== "object" || Array.isArray(body))) {
    throw new Error("gRPC request body must be a JSON object")
  }
  const request: Record<string, unknown> = { directory, ...(body as Record<string, unknown> | undefined) }
  for (const [key, value] of url.searchParams) {
    const fields = key.split(".").map(camel)
    let target = request
    for (const field of fields.slice(0, -1)) {
      if (!target[field] || typeof target[field] !== "object" || Array.isArray(target[field])) target[field] = {}
      target = target[field] as Record<string, unknown>
    }
    target[fields.at(-1) ?? key] = value
  }
  match.row.placeholders.forEach((field, index) => {
    request[field] = decodeURIComponent(match.segments?.[index + 1] ?? "")
  })
  return { service: match.row.service, method: match.row.rpcMethod, request, streaming: match.row.streaming }
}

export interface GrpcWire {
  unary(service: string, method: string, request: Record<string, unknown>): Promise<unknown>
  stream(
    service: string, method: string, request: Record<string, unknown>,
    onFrame: (frame: StreamFrame) => void | Promise<void>, signal: AbortSignal,
  ): Promise<void>
  close(): void
}

interface WireMethod {
  path: string
  requestSerialize(value: Record<string, unknown>): Buffer
  responseDeserialize(buffer: Buffer): unknown
  requestStream: boolean
  responseStream: boolean
}

class NativeGrpcWire implements GrpcWire {
  private readonly client: grpc.Client
  private readonly definitions: protoLoader.PackageDefinition

  constructor(endpoint: string) {
    const protoRoot = resolve(import.meta.dir, "../../../proto")
    const protoDir = resolve(protoRoot, "hya/v1")
    this.definitions = protoLoader.loadSync(
      readdirSync(protoDir).filter((name) => name.endsWith(".proto")).map((name) => resolve(protoDir, name)),
      { includeDirs: [protoRoot], keepCase: false, longs: String, enums: String, defaults: false },
    )
    this.client = new grpc.Client(endpoint, grpc.credentials.createInsecure())
  }

  private method(service: string, name: string): WireMethod {
    const definition = this.definitions[`hya.v1.${service}`] as Record<string, WireMethod> | undefined
    const method = definition?.[name]
    if (!method) throw new Error(`Unknown hya.v1 gRPC method ${service}.${name}`)
    return method
  }

  unary(service: string, name: string, request: Record<string, unknown>): Promise<unknown> {
    const method = this.method(service, name)
    if (method.requestStream || method.responseStream) throw new Error(`${service}.${name} streams; use a live client`)
    return new Promise((resolve, reject) => {
      this.client.makeUnaryRequest(
        method.path, method.requestSerialize, method.responseDeserialize, request, new grpc.Metadata(),
        (error, response) => error ? reject(error) : resolve(response),
      )
    })
  }

  stream(
    service: string, name: string, request: Record<string, unknown>,
    onFrame: (frame: StreamFrame) => void | Promise<void>, signal: AbortSignal,
  ): Promise<void> {
    const method = this.method(service, name)
    if (method.requestStream || !method.responseStream) throw new Error(`${service}.${name} is not a server stream`)
    return new Promise((resolve, reject) => {
      const call = this.client.makeServerStreamRequest(
        method.path, method.requestSerialize, method.responseDeserialize,
        request, new grpc.Metadata(),
      )
      let settled = false
      let pending = Promise.resolve()
      const finish = (error?: unknown): void => {
        if (settled) return
        settled = true
        signal.removeEventListener("abort", abort)
        if (error) reject(error)
        else resolve()
      }
      const abort = (): void => { call.cancel() }
      signal.addEventListener("abort", abort, { once: true })
      if (signal.aborted) abort()
      call.on("data", (frame: StreamFrame) => {
        call.pause()
        pending = pending.then(() => onFrame(frame)).then(() => {
          if (!signal.aborted) call.resume()
        })
        pending.catch((error: unknown) => { call.cancel(); finish(error) })
      })
      call.on("error", (error: grpc.ServiceError) => {
        if (signal.aborted && error.code === grpc.status.CANCELLED) finish()
        else finish(error)
      })
      call.on("end", () => { void pending.then(() => finish(), finish) })
    })
  }

  close(): void { this.client.close() }
}

const httpStatusByGrpcCode: Record<number, number> = {
  [grpc.status.INVALID_ARGUMENT]: 400,
  [grpc.status.NOT_FOUND]: 404,
  [grpc.status.PERMISSION_DENIED]: 403,
  [grpc.status.FAILED_PRECONDITION]: 409,
  [grpc.status.UNAVAILABLE]: 503,
}

/** Reuse the TUI workflows while connecting to the separate gRPC listener. */
export class GrpcHyaClient extends HyaClient {
  private readonly wire: GrpcWire

  constructor(endpoint: string, directory: string, wire?: GrpcWire) {
    super("http://hya.invalid", directory)
    this.wire = wire ?? new NativeGrpcWire(endpoint)
  }

  override async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const operation = resolveGrpcOperation(method, path, body, this.directory)
    if (operation.streaming) throw new Error("This endpoint streams events; open a session to view its live updates")
    try {
      return await this.wire.unary(operation.service, operation.method, operation.request) as T
    } catch (error) {
      const serviceError = error as { code?: unknown; details?: unknown }
      if (typeof serviceError.code !== "number") throw error
      const status = httpStatusByGrpcCode[serviceError.code] ?? 500
      throw new HttpError(status, method, path, String(serviceError.details ?? error))
    }
  }

  override streamSession(
    session: string, sinceSeq: string,
    onFrame: (frame: StreamFrame) => void | Promise<void>, signal: AbortSignal,
  ): Promise<void> {
    return this.wire.stream("Events", "StreamSessionEvents", { session, sinceSeq }, onFrame, signal)
  }

  close(): void { this.wire.close() }
}
