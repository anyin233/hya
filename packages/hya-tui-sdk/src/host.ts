/**
 * The extension's isolation boundary: the bundled extension and the SDK
 * runner execute inside a QuickJS VM compiled to WebAssembly. The VM has only
 * ECMAScript built-ins, a `console` (to the host's log), and one host bridge,
 * `__hyaHost(method, paramsJson)`, through which the SDK's `api.fs` reaches the
 * TUI. No file system, network, process, timers, or Bun/Node APIs. Its memory
 * is one capped `WebAssembly.Memory` (QuickJS's own limit is not a hard cap),
 * and VM execution runs under a deadline enforced by QuickJS's interrupt hook.
 *
 * Host side (Bun): bundle the entry with `Bun.build` (relative imports and
 * `@hya/tui-sdk` only), then pass each JSON-RPC frame as a string to the VM.
 */
import { dirname, resolve } from "node:path"
import { newQuickJSWASMModuleFromVariant, newVariant, shouldInterruptAfterDeadline, type QuickJSContext, type QuickJSHandle, type QuickJSRuntime } from "quickjs-emscripten-core"
import releaseSync from "@jitl/quickjs-wasmfile-release-sync"
import { VM_DEADLINE_MS, VM_STOPPED_ERROR_CODE } from "./protocol"

/** Hard cap of one extension VM's memory. */
export const VM_MEMORY_BYTES = 64 * 1024 * 1024

const WASM_PAGE = 64 * 1024
/** The QuickJS build's declared minimum memory (pages): a VM's memory starts at least this large. */
const QUICKJS_MIN_PAGES = 256
const sdkSource = resolve(import.meta.dir)
const entryModule = "hya:extension-entry"

export class ExtensionBuildError extends Error {
  override name = "ExtensionBuildError"
}

/** A call from the VM to its host (the SDK's `api.fs`); resolves to the JSON value the VM receives. */
export type HostCall = (method: string, params: Record<string, unknown>) => Promise<unknown>

/**
 * Bundle `entry` and the SDK runner into one script that installs
 * `globalThis.__hyaHandle(frameJson) → Promise<responseJson>`.
 */
export async function bundleExtension(entry: string): Promise<string> {
  const absolute = resolve(entry)
  const result = await Bun.build({
    entrypoints: [entryModule],
    target: "browser",
    format: "iife",
    plugins: [{
      name: "hya-tui-extension",
      setup(build) {
        build.onResolve({ filter: /^hya:extension-entry$/ }, () => ({ path: "entry", namespace: "hya" }))
        build.onLoad({ filter: /.*/, namespace: "hya" }, () => ({
          loader: "ts",
          contents: [
            `import extension from ${JSON.stringify(absolute)}`,
            `import { TuiExtensionRunner } from ${JSON.stringify(resolve(sdkSource, "runtime.ts"))}`,
            "const host = (method, params) => globalThis.__hyaHost(method, JSON.stringify(params)).then(JSON.parse)",
            "const runner = new TuiExtensionRunner(extension, host)",
            "globalThis.__hyaHandle = async (json) => JSON.stringify(await runner.handle(JSON.parse(json)))",
          ].join("\n"),
        }))
        build.onResolve({ filter: /^@hya\/tui-sdk$/ }, () => ({ path: resolve(sdkSource, "index.ts") }))
        // Anything else that is not a relative or absolute path: a package or a runtime module, unavailable in the VM.
        build.onResolve({ filter: /^[^./]/ }, (args) => {
          if (args.namespace === "hya" || args.path.startsWith("@hya/tui-sdk")) return undefined
          throw new ExtensionBuildError(`cannot import ${JSON.stringify(args.path)}: extensions run in an isolated VM; only relative imports and @hya/tui-sdk are available`)
        })
      },
    }],
    root: dirname(absolute),
  }).catch((error: unknown) => {
    throw new ExtensionBuildError(error instanceof AggregateError ? error.errors.map(String).join("; ") : String(error))
  })
  if (!result.success || !result.outputs[0]) throw new ExtensionBuildError(result.logs.map(String).join("; ") || "bundle failed")
  return await result.outputs[0].text()
}

/**
 * The QuickJS WebAssembly module, compiled once per thread: every VM
 * instantiates it with its own memory. The shared host's worker calls this at
 * boot so the compile overlaps the TUI's catalog fetch and bundling.
 */
let compiled: Promise<WebAssembly.Module> | undefined
export function quickJsModule(): Promise<WebAssembly.Module> {
  compiled ??= Bun.file(Bun.resolveSync("@jitl/quickjs-wasmfile-release-sync/wasm", import.meta.dir)).arrayBuffer().then((bytes) => WebAssembly.compile(bytes))
  return compiled
}

export interface VmOptions {
  readonly memoryBytes?: number
  readonly deadlineMs?: number
  readonly log?: (line: string) => void
  /** Serves `api.fs`; without it every host call rejects. */
  readonly hostCall?: HostCall
}

/** One extension's VM: `handle` evaluates one JSON-RPC request frame and resolves to the response frame. */
export class ExtensionVm {
  /** Host calls the VM is waiting for; `handle` awaits them between job runs. */
  private readonly outstanding = new Set<Promise<void>>()

  /** A request is running (it runs released jobs itself). */
  private handling = false
  private disposed = false
  /**
   * A handler ran past the deadline: its state may be half-updated, and every
   * further request could spin again on the thread other VMs share. Later
   * requests answer the deadline error at once; the TUI restarts the extension.
   */
  private exceeded = false

  private constructor(
    private readonly runtime: QuickJSRuntime,
    private readonly vm: QuickJSContext,
    private readonly deadlineMs: number,
  ) {}

  static async create(script: string, options: VmOptions = {}): Promise<ExtensionVm> {
    const pages = Math.ceil((options.memoryBytes ?? VM_MEMORY_BYTES) / WASM_PAGE)
    const memory = new WebAssembly.Memory({ initial: Math.min(QUICKJS_MIN_PAGES, pages), maximum: Math.max(QUICKJS_MIN_PAGES, pages) })
    const module = await newQuickJSWASMModuleFromVariant(newVariant(releaseSync, { wasmMemory: memory, wasmModule: await quickJsModule() }))
    const runtime = module.newRuntime()
    runtime.setMaxStackSize(512 * 1024)
    const vm = runtime.newContext()
    const instance = new ExtensionVm(runtime, vm, options.deadlineMs ?? VM_DEADLINE_MS)
    instance.installConsole(options.log ?? ((line) => process.stderr.write(`${line}\n`)))
    instance.installHost(options.hostCall)
    runtime.setInterruptHandler(shouldInterruptAfterDeadline(Date.now() + instance.deadlineMs))
    const loaded = vm.evalCode(script, "extension.js")
    if (loaded.error) {
      const error = vm.dump(loaded.error)
      loaded.error.dispose()
      instance.dispose()
      throw new ExtensionBuildError(`extension failed to load: ${typeof error === "object" && error ? `${error.name}: ${error.message}` : String(error)}`)
    }
    loaded.value.dispose()
    return instance
  }

  private installConsole(log: (line: string) => void): void {
    const { vm } = this
    const console = vm.newObject()
    const write = vm.newFunction("log", (...args) => {
      log(args.map((arg) => { const value = vm.dump(arg); return typeof value === "string" ? value : JSON.stringify(value) }).join(" ").slice(0, 4_000))
    })
    for (const name of ["log", "info", "warn", "error", "debug"]) vm.setProp(console, name, write)
    vm.setProp(vm.global, "console", console)
    write.dispose()
    console.dispose()
  }

  /** `__hyaHost(method, paramsJson)` → a VM promise settled by the host's answer (a JSON string). */
  private installHost(hostCall: HostCall | undefined): void {
    const { vm } = this
    const host = vm.newFunction("__hyaHost", (methodHandle, paramsHandle) => {
      const method = vm.dump(methodHandle)
      const params = vm.dump(paramsHandle)
      const deferred = vm.newPromise()
      const call = typeof method === "string" && typeof params === "string" && hostCall
        ? hostCall(method, JSON.parse(params) as Record<string, unknown>)
        : Promise.reject(new Error("host unavailable"))
      const settled = call.then(
        (value) => { if (this.disposed) return; const json = vm.newString(JSON.stringify(value ?? null)); deferred.resolve(json); json.dispose() },
        (error: unknown) => { if (this.disposed) return; const reason = vm.newError(error instanceof Error ? error.message : String(error)); deferred.reject(reason); reason.dispose() },
      ).finally(() => {
        this.outstanding.delete(settled)
        // Settled between requests (a call an earlier handler did not await): run what it released now.
        if (!this.handling && !this.disposed) this.drain()
      })
      this.outstanding.add(settled)
      return deferred.handle
    })
    vm.setProp(vm.global, "__hyaHost", host)
    host.dispose()
  }

  /**
   * Run one request: execute the handler and its jobs; while it waits for
   * host calls, await them (that wait is outside the deadline) and run the
   * jobs they released. A handler still pending with nothing outstanding can
   * never settle and is answered with an error.
   */
  async handle(frameJson: string, id: string | number | null): Promise<string> {
    if (this.exceeded) return JSON.stringify({ jsonrpc: "2.0", id, error: this.deadlineError() })
    this.handling = true
    try {
      return await this.run(frameJson, id)
    } finally {
      this.handling = false
    }
  }

  /** Run the jobs released between requests; a failure there belongs to no request and is dropped. */
  private drain(): void {
    this.runtime.setInterruptHandler(shouldInterruptAfterDeadline(Date.now() + this.deadlineMs))
    const jobs = this.runtime.executePendingJobs()
    if (jobs.error) this.describe(jobs.error)
  }

  private async run(frameJson: string, id: string | number | null): Promise<string> {
    const { vm, runtime } = this
    const failure = (error: { code: number; message: string }) => JSON.stringify({ jsonrpc: "2.0", id, error })
    const fault = (message: string) => failure({ code: -32000, message })
    const run = <T>(work: () => T): T => {
      runtime.setInterruptHandler(shouldInterruptAfterDeadline(Date.now() + this.deadlineMs))
      return work()
    }
    const handler = vm.getProp(vm.global, "__hyaHandle")
    const argument = vm.newString(frameJson)
    const called = run(() => vm.callFunction(handler, vm.undefined, argument))
    argument.dispose()
    handler.dispose()
    if (called.error) return failure(this.describe(called.error))
    const promise = called.value
    try {
      for (;;) {
        for (;;) {
          const jobs = run(() => runtime.executePendingJobs())
          if (jobs.error) return failure(this.describe(jobs.error))
          if (jobs.value === 0) break
        }
        const state = vm.getPromiseState(promise)
        if (state.type === "rejected") return failure(this.describe(state.error))
        if (state.type === "fulfilled") {
          const response = vm.dump(state.value)
          state.value.dispose()
          return typeof response === "string" ? response : fault("handler returned no response")
        }
        if (this.outstanding.size === 0) return fault("handler never settles (extensions have no timers; only api.fs is asynchronous)")
        await Promise.race(this.outstanding)
      }
    } finally {
      promise.dispose()
    }
  }

  dispose(): void {
    this.disposed = true
    this.vm.dispose()
    this.runtime.dispose()
  }

  private describe(handle: QuickJSHandle): { code: number; message: string } {
    const error = this.vm.dump(handle)
    handle.dispose()
    const message = error && typeof error === "object" && "message" in error ? String(error.message) : String(error)
    if (message !== "interrupted") return { code: -32000, message }
    this.exceeded = true
    return this.deadlineError()
  }

  private deadlineError(): { code: number; message: string } {
    return { code: VM_STOPPED_ERROR_CODE, message: `extension exceeded its ${this.deadlineMs} ms limit` }
  }
}
