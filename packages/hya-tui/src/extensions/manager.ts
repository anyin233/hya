/**
 * Runs the bundle TUI extensions of the current backend scope and holds what
 * they contribute. app/run.tsx feeds it the `ListTuiExtensions` catalog;
 * app/App.tsx feeds it the host context; views read it through Solid signals
 * (one per concern, so a panel refresh does not re-run tool cards).
 *
 * Every extension runs in one shared host process (extensions/hostChannel.ts;
 * packages/hya-tui-sdk/src/main.ts) from files verified against the catalog's
 * sha256 (extensions/install.ts), each in its own QuickJS-WASM VM; where the
 * OS allows, the host is also confined by the operating system's sandbox
 * (extensions/sandbox.ts, extensions/confine.ts). `api.fs` reads go through
 * extensions/files.ts. Views never wait: a surface shows the built-in
 * rendering until the extension's answer arrives, and any failure (timeout,
 * crash, invalid tree) falls back to it. An extension that times out, or all
 * of them when the host dies, restarts at most `restartBudget` times per
 * `restartWindowMs`.
 */
import { mkdir } from "node:fs/promises"
import { join } from "node:path"
import { createSignal, type Accessor, type Setter } from "solid-js"
import { locateSdk, materialize, parseCatalogEntry, sdkCompatible, type CatalogExtension, type InstalledSdk } from "./install"
import { ExtensionFiles, type FsEvent } from "./files"
import { HostChannel, HostTimeoutError, type Spawn } from "./hostChannel"
import { planSandbox, type SandboxPolicy } from "./sandbox"
import { errorLine } from "../state/projectCommands"
import {
  extensionApiVersion, extensionStoppedCode, nodeText, safeText, scopeContext, validateContributions, validateDecision, validateRenderNode,
  type CommandResult, type Contributions, type ExtensionAction, type ExtensionContext, type ExtensionPermission, type HostCommand, type JsonValue, type NodeAction, type PanelContribution,
  type RenderNode, type RendererContribution, type ReplaceableSurface,
} from "./wire"

export type ExtensionState = "starting" | "running" | "failed" | "disabled" | "blocked"

/** One catalog extension as `/extensions` shows it. */
export interface ExtensionInfo {
  readonly id: string
  readonly version: string
  readonly state: ExtensionState
  readonly reason?: string
  /** The OS sandbox confines the extension host. */
  readonly isolated: boolean
  /** Trusted: runs on JavaScriptCore's JIT on its own thread instead of in the QuickJS VM. */
  readonly jit: boolean
  readonly permissions: readonly ExtensionPermission[]
  readonly contributions?: Contributions
  readonly warnings: readonly string[]
  readonly log: readonly string[]
}

/** A panel from any running extension; `key` is `<bundle id>#<panel id>`. */
export interface PanelEntry extends PanelContribution { readonly key: string; readonly extension: string }

/** What a view shows for a surface: a validated tree, an error, or nothing yet. */
export interface SurfaceView { readonly node?: RenderNode; readonly error?: string }

export interface ToolCallInput {
  readonly id: string
  readonly tool: string
  readonly status: "pending" | "running" | "completed" | "error"
  readonly summary: string
  readonly input?: JsonValue
  readonly output?: JsonValue
  readonly error?: string
}

/** One renderer's tree and who answers its actions. */
export interface HookTree { readonly node: RenderNode; readonly extension: string; readonly renderer: string }
/** Built-in rendering wrapped by extensions: an optional replacement, then decorators innermost first. */
export interface HookTrees { readonly replace?: HookTree; readonly decorators: readonly HookTree[] }

export interface ManagerOptions {
  readonly cacheRoot: string
  readonly sdk?: InstalledSdk
  readonly sandbox?: SandboxPolicy
  /** The bun binary that runs extensions; defaults to the one running the TUI. */
  readonly bun?: string
  readonly spawn?: Spawn
  readonly timeoutMs?: number
  readonly restartBudget?: number
  readonly restartWindowMs?: number
  /** Status-line notices (action results, failures). */
  readonly notice?: (text: string) => void
  /** Host UI callbacks: end keyboard capture (`ui.release`), close the overlay (`ui.close`), show a panel as the overlay again (`ui.open`). */
  readonly release?: () => void
  readonly close?: (panelKey: string) => void
  readonly open?: (panelKey: string) => void
  /** Run a host command an extension answered with (`ui.*` commands the manager runs itself); a token's result is the returned value or the thrown error. */
  readonly executeCommand?: (command: HostCommand, origin: CommandOrigin) => Promise<JsonValue | undefined> | JsonValue | undefined
}

/** Where a host command came from: the surface whose handler answered with it, and the click that triggered it. */
export interface CommandOrigin {
  readonly surface: { readonly kind: "panel" | "renderer"; readonly id: string }
  readonly extension: string
  readonly point?: { readonly x: number; readonly y: number }
}

export interface LoadOptions {
  /** User preference: `false` keeps an extension stopped; remote extensions need an explicit `true`. */
  readonly enabled: (id: string) => boolean | undefined
  /** The backend is remote (`hya --connect`, `/connect-remote`): its code runs only when enabled explicitly. */
  readonly remote: boolean
  /**
   * User preference (`/extensions trust|untrust`): `true` runs an extension on
   * the JIT tier, `false` keeps it in the VM. Unset: first-party bundles of a
   * local backend run on the JIT tier.
   */
  readonly trusted: (id: string) => boolean | undefined
}

interface Running {
  readonly catalog: CatalogExtension
  state: ExtensionState
  reason?: string
  isolated: boolean
  /** Runs on the JIT tier (see `LoadOptions.trusted`). */
  jit: boolean
  warnings: string[]
  /** The latest lines of its console output, newest last. */
  log: string[]
  contributions?: Contributions
  starts: number[]
  /** Bumped when the extension asks for a redraw; every cached surface of an older generation is stale. */
  generation: number
  surfaces: Map<string, { readonly key: string; readonly view: SurfaceView }>
  inflight: Set<string>
  timers: Timer[]
  restartTimer?: Timer
  /** Settles when the current start attempt ends (running or failed). */
  ready: Promise<void>
  /** JSON of the scoped context the extension last received. */
  sentContext?: string
}

const contextIntervalMs = 200
const maxCached = 500
const emptyContext: ExtensionContext = { terminal: { columns: 0, rows: 0 } }
/** The first-party bundle that draws the panes the TUI no longer builds in (Sessions, Todos, Projects, Context). */
const basicComponentsBundle = "hya/basic-tui-components"

function signal(): [Accessor<number>, () => void] {
  const [read, write]: [Accessor<number>, Setter<number>] = createSignal(0)
  return [read, () => write((value) => value + 1)]
}

export class ExtensionManager {
  private options: ManagerOptions
  private readonly running = new Map<string, Running>()
  private context: ExtensionContext = emptyContext
  private contextTimer: Timer | undefined
  /** The shared extension host, once started; `hostStart` while it is being planned and spawned. */
  private host: HostChannel | undefined
  private hostStart: Promise<HostChannel> | undefined
  /** The host runs inside the OS sandbox (the VM boundary always applies). */
  private hostIsolated = false
  private hostWarning: string | undefined
  private requestId = 0
  private readonly files = new ExtensionFiles((ext, watch, events) => void this.fsEvent(ext, watch, events))
  private readonly formatCache = new Map<string, string>()
  private readonly formatInflight = new Set<string>()
  private readonly keyQueues = new Map<string, Promise<void>>()
  private loadSequence = 0
  /** A catalog has been loaded at least once (until then every surface is still loading). */
  private loaded = false
  /** Why the last catalog read failed (cleared by the next successful load). */
  private catalogError: string | undefined
  /** `settled()` callers waiting for the first catalog and hya/basic-tui-components to start. */
  private settledWaiters: (() => void)[] = []
  /** Extension list, states, and contributions. */
  readonly listVersion: Accessor<number>
  private readonly bumpList: () => void
  /** Panel and status trees. */
  readonly surfaceVersion: Accessor<number>
  private readonly bumpSurfaces: () => void
  /** Tool-call and composer hook trees. */
  readonly hookVersion: Accessor<number>
  private readonly bumpHooks: () => void
  /** Formatted tool output. */
  readonly formatVersion: Accessor<number>
  private readonly bumpFormat: () => void

  constructor(options: ManagerOptions) {
    this.options = options
    const [listVersion, bumpList] = signal()
    this.listVersion = listVersion
    this.bumpList = () => {
      bumpList()
      if (!this.settledWaiters.length || !this.isSettled()) return
      for (const resolve of this.settledWaiters.splice(0)) resolve()
    };
    [this.surfaceVersion, this.bumpSurfaces] = signal();
    [this.hookVersion, this.bumpHooks] = signal();
    [this.formatVersion, this.bumpFormat] = signal()
  }

  private isSettled(): boolean {
    return this.loaded && this.running.get(basicComponentsBundle)?.state !== "starting"
  }

  /** Resolves once a catalog is loaded and hya/basic-tui-components has started (or could not). */
  settled(): Promise<void> {
    if (this.isSettled()) return Promise.resolve()
    const { promise, resolve } = Promise.withResolvers<void>()
    this.settledWaiters.push(resolve)
    return promise
  }

  configure(options: Partial<ManagerOptions>): void {
    this.options = { ...this.options, ...options }
  }

  /** The SDK version extensions run against (`undefined`: no SDK next to this TUI). */
  sdkVersion(): string | undefined { return this.options.sdk?.version }

  sandboxPolicy(): SandboxPolicy { return this.options.sandbox ?? "best-effort" }

  /** Every catalog extension of the current scope, sorted by id. */
  list(): ExtensionInfo[] {
    this.listVersion()
    return [...this.running.values()].map((entry) => ({
      id: entry.catalog.bundleId, version: entry.catalog.bundleVersion, state: entry.state,
      ...(entry.reason ? { reason: entry.reason } : {}), isolated: entry.isolated, jit: entry.jit,
      permissions: entry.catalog.permissions, ...(entry.contributions ? { contributions: entry.contributions } : {}),
      warnings: entry.warnings, log: entry.log,
    }))
  }

  /**
   * Replace the loaded set with `entries` (the raw catalog): unchanged
   * extensions (same prepared digest) keep running, the rest stop or start.
   * Returns the catalog rows that were rejected, with why.
   */
  async load(entries: readonly unknown[], options: LoadOptions): Promise<string[]> {
    const sequence = ++this.loadSequence
    const rejected: string[] = []
    const wanted = new Map<string, CatalogExtension>()
    for (const entry of entries) {
      const parsed = parseCatalogEntry(entry)
      if (typeof parsed === "string") rejected.push(parsed)
      else wanted.set(parsed.bundleId, parsed)
    }
    for (const [id, entry] of this.running) {
      const next = wanted.get(id)
      if (next && next.preparedDigest === entry.catalog.preparedDigest && this.allowed(id, options) === (entry.state !== "disabled" && entry.state !== "blocked") && this.trusted(next, options) === entry.jit) {
        wanted.delete(id)
        continue
      }
      this.running.delete(id)
      await this.shutdown(entry)
    }
    if (sequence !== this.loadSequence) return rejected
    const starts: Promise<void>[] = []
    for (const catalog of [...wanted.values()].sort((a, b) => a.bundleId.localeCompare(b.bundleId))) {
      const entry: Running = { catalog, state: "starting", isolated: false, jit: this.trusted(catalog, options), warnings: [], log: [], starts: [], generation: 0, surfaces: new Map(), inflight: new Set(), timers: [], ready: Promise.resolve() }
      this.running.set(catalog.bundleId, entry)
      const allowed = this.allowed(catalog.bundleId, options)
      if (allowed === false) {
        entry.state = options.remote && options.enabled(catalog.bundleId) === undefined ? "blocked" : "disabled"
        entry.reason = entry.state === "blocked" ? `from a remote backend: run /extensions enable ${catalog.bundleId} to trust it` : "disabled by preference"
        continue
      }
      starts.push(this.start(entry))
    }
    this.sortRunning()
    this.loaded = true
    this.catalogError = undefined
    this.bumpAll()
    await Promise.all(starts)
    // Prewarmed for a catalog that runs nothing: do not keep idle workers around.
    if (sequence === this.loadSequence && ![...this.running.values()].some((entry) => entry.state === "running" || entry.state === "starting")) await this.stopHost()
    return rejected
  }

  /**
   * Start the shared host (sandbox plan, process, worker compile) while the
   * catalog is still in flight; `load` stops it again when nothing runs.
   * Settles once the host is up; a failure surfaces at the first start.
   */
  async prewarm(): Promise<void> {
    const sdk = this.options.sdk
    if (sdk) await this.ensureHost(sdk).catch(() => undefined)
  }

  /**
   * Feed the latest host context. Running extensions receive it (scoped to
   * their permissions) at most every `contextIntervalMs`, and only when their
   * scoped view of it changed.
   */
  setContext(context: ExtensionContext): void {
    this.context = context
    if (this.contextTimer) return
    this.contextTimer = setTimeout(() => {
      this.contextTimer = undefined
      // Scoped and serialized once per permission set, however many extensions share it.
      const scoped = new Map<string, { context: ExtensionContext; json: string }>()
      for (const entry of this.running.values()) void this.pushContext(entry, scoped)
    }, contextIntervalMs)
  }

  /** The active Project's roots (the first is primary): what `api.fs` may read. */
  setRoots(roots: readonly string[]): void {
    this.files.setRoots(roots)
  }

  async stopAll(): Promise<void> {
    clearTimeout(this.contextTimer)
    const entries = [...this.running.values()]
    this.running.clear()
    this.bumpAll()
    await Promise.all(entries.map((entry) => this.shutdown(entry)))
    await this.stopHost()
  }

  private async stopHost(): Promise<void> {
    const host = this.host ?? await this.hostStart?.catch(() => undefined)
    this.host = undefined
    this.hostStart = undefined
    await host?.stop()
  }

  /** Restart one extension (resets its restart budget). */
  async reload(id: string): Promise<void> {
    const entry = this.running.get(id)
    if (!entry) throw new Error(`No extension ${id}`)
    await this.shutdown(entry)
    entry.starts = []
    await this.start(entry)
  }

  /** Start or stop one extension now (the caller persists the preference). */
  async setEnabled(id: string, enabled: boolean): Promise<void> {
    const entry = this.running.get(id)
    if (!entry) throw new Error(`No extension ${id}`)
    if (!enabled) {
      await this.shutdown(entry)
      entry.state = "disabled"
      entry.reason = "disabled by preference"
      this.bumpAll()
      return
    }
    if (entry.state === "disabled" || entry.state === "blocked" || entry.state === "failed") {
      entry.starts = []
      await this.start(entry)
    }
  }

  /** Move one extension to the JIT tier or back to the VM, restarting it if it runs (the caller persists the preference). */
  async setTrusted(id: string, trusted: boolean): Promise<void> {
    const entry = this.running.get(id)
    if (!entry) throw new Error(`No extension ${id}`)
    if (entry.jit === trusted) return
    entry.jit = trusted
    this.bumpList()
    if (entry.state === "running" || entry.state === "starting" || entry.state === "failed") await this.reload(id)
  }

  // Views ------------------------------------------------------------------

  /** Panels of running extensions, sorted by key. */
  panels(): PanelEntry[] {
    this.listVersion()
    const out: PanelEntry[] = []
    for (const entry of this.running.values()) {
      if (entry.state !== "running") continue
      for (const panel of entry.contributions?.panels ?? []) out.push({ ...panel, key: `${entry.catalog.bundleId}#${panel.id}`, extension: entry.catalog.bundleId })
    }
    return out
  }

  /** The panel replacing a surface; non-first-party wins, then bundle id. */
  replacement(kind: ReplaceableSurface): PanelEntry | undefined {
    return this.panels().filter((panel) => panel.replaces === kind).sort((a, b) => {
      const ae = this.running.get(a.extension)?.catalog.firstParty ? 1 : 0
      const be = this.running.get(b.extension)?.catalog.firstParty ? 1 : 0
      return ae - be || a.extension.localeCompare(b.extension)
    })[0]
  }

  /**
   * What a built-in surface shows while no extension replaces it:
   * `undefined` while the catalog or hya/basic-tui-components is still
   * starting, else why the panes are missing.
   */
  placeholder(surface: ReplaceableSurface): string | undefined {
    this.listVersion()
    const entry = this.running.get(basicComponentsBundle)
    if (!this.loaded || entry?.state === "starting") return undefined
    const pane = surface === "context_line" ? "Context" : surface === "project_view" ? "Projects" : surface[0]!.toUpperCase() + surface.slice(1)
    const reason = !entry ? this.catalogError ?? "not installed" : entry.state === "running" ? `it has no ${surface} panel` : `${entry.state}${entry.reason ? `: ${entry.reason}` : ""}`
    return `${pane} needs ${basicComponentsBundle} (${reason})`
  }

  /** The catalog could not be read: surfaces stop loading and say why; running extensions keep running. */
  catalogFailed(reason: string): void {
    this.loaded = true
    this.catalogError = `extension catalog unavailable: ${reason}`
    this.bumpList()
  }

  /** A panel's current tree at this size; asks the extension when stale. */
  panelView(key: string, width: number, height: number): SurfaceView {
    this.surfaceVersion()
    const [extension, id] = splitKey(key)
    const entry = this.running.get(extension)
    if (!entry || entry.state !== "running" || !entry.contributions?.panels.some((panel) => panel.id === id)) return { error: `Extension panel ${key} is not available` }
    const cacheKey = `${entry.generation}:${width}x${height}`
    const slot = `panel:${id}`
    const cached = entry.surfaces.get(slot)
    if (cached?.key !== cacheKey && !entry.inflight.has(slot)) {
      entry.inflight.add(slot)
      void this.renderSurface(entry, { kind: "panel", id }, width, height, undefined, "forbid").then((view) => {
        entry.inflight.delete(slot)
        // An identical tree keeps the previous object: the view (and the row under the mouse) is not rebuilt.
        const previous = entry.surfaces.get(slot)?.view
        const same = previous !== undefined && JSON.stringify(previous) === JSON.stringify(view)
        entry.surfaces.set(slot, { key: cacheKey, view: same ? previous : view })
        // A newer generation arrived while this one rendered: let the view ask again.
        if (!same || cacheKey !== `${entry.generation}:${width}x${height}`) this.bumpSurfaces()
      })
    }
    return cached?.view ?? {}
  }

  /** Context fields contributed by running extensions, consumed as `context.status.items`. */
  statusFields(): { label: string; value: string; priority: number }[] {
    this.surfaceVersion()
    const fields: { label: string; value: string; priority: number }[] = []
    for (const entry of this.running.values()) {
      if (entry.state !== "running") continue
      for (const item of entry.contributions?.statusItems ?? []) {
        const slot = `status:${item.id}`
        const cacheKey = String(entry.generation)
        const cached = entry.surfaces.get(slot)
        if (cached?.key !== cacheKey && !entry.inflight.has(slot)) {
          entry.inflight.add(slot)
          void this.renderSurface(entry, { kind: "status", id: item.id }, 0, 0, undefined, "forbid").then((view) => {
            entry.inflight.delete(slot)
            entry.surfaces.set(slot, { key: cacheKey, view })
            this.bumpSurfaces()
          })
        }
        const text = cached?.view.node ? nodeText(cached.view.node).replace(/\s+/g, " ").trim().slice(0, 80) : ""
        if (text) fields.push({ label: item.label, value: text, priority: item.priority })
      }
    }
    return fields
  }

  /** Extension trees around one tool card; `undefined` parts keep the built-in card. */
  toolCall(input: ToolCallInput): HookTrees {
    this.hookVersion()
    return this.hookTrees("tool_call", `${input.id}:${Bun.hash(JSON.stringify([input.status, input.summary, input.output, input.error]))}`, 0, input)
  }

  /** Decorations around the composer (the composer itself always stays the host's). */
  composer(width: number): HookTrees {
    this.hookVersion()
    return this.hookTrees("composer", `w${width}`, width, undefined)
  }

  /** Extension pretty printing of a tool output; `base` until the formatters answer. */
  format(value: unknown, base: string): string {
    this.formatVersion()
    const formatters = this.ordered((contributions) => contributions.formatters)
    if (formatters.length === 0 || base.length > 64 * 1024) return base
    const key = `${formatters.map(({ entry }) => `${entry.catalog.bundleId}@${entry.generation}`).join(",")}:${Bun.hash(JSON.stringify(value) ?? "")}`
    const cached = this.formatCache.get(key)
    if (cached !== undefined) return cached
    if (!this.formatInflight.has(key)) {
      this.formatInflight.add(key)
      void (async () => {
        let text = base
        for (const { entry, item } of formatters) {
          const result = await this.call<{ text?: unknown }>(entry, "tui/format", { formatter: item.id, input: { value: (value ?? null) as JsonValue, text }, width: this.context.terminal.columns })
          const next = result && result.text !== null ? safeText(result.text) : undefined
          if (next !== undefined) text = next
        }
        this.formatInflight.delete(key)
        if (this.formatCache.size >= maxCached) this.formatCache.clear()
        this.formatCache.set(key, text)
        this.bumpFormat()
      })()
    }
    return base
  }

  /**
   * Run submit interceptors, highest priority first. A failing or slow
   * interceptor continues: an extension can never make sending impossible
   * except by answering `block`.
   */
  async interceptSubmit(text: string): Promise<{ text: string } | { blocked: string }> {
    let current = text
    for (const { entry, item } of this.ordered((contributions) => contributions.interceptors).reverse()) {
      const decision = validateDecision(await this.call(entry, "tui/intercept", { interceptor: item.id, input: { text: current } }))
      if (decision.decision === "block") return { blocked: `${entry.catalog.bundleId}: ${decision.message}` }
      if (decision.decision === "replace") current = decision.text
    }
    return { text: current }
  }

  /** A click on a node action inside a panel or renderer tree. */
  async action(extension: string, surface: { kind: "panel" | "renderer"; id: string }, action: NodeAction | ExtensionAction, point?: { x: number; y: number }): Promise<void> {
    const entry = this.running.get(extension)
    if (!entry || entry.state !== "running" || !entry.catalog.permissions.includes("tui.action")) return
    const wireAction: ExtensionAction = { ...action, button: "button" in action ? action.button : "left" }
    const result = await this.call(entry, "tui/action", { surface, action: wireAction })
    await this.handled(entry, "tui/action", result, { surface, extension, ...(point ? { point } : {}) })
  }

  /** Send a captured key to a panel, serializing requests for each surface (a terminal key event carries more than the wire's five fields). */
  key(panelKey: string, pressed: { readonly name: string; readonly sequence: string; readonly ctrl: boolean; readonly shift: boolean; readonly meta: boolean }): Promise<void> {
    const [extension, id] = splitKey(panelKey)
    const key = { name: pressed.name, sequence: pressed.sequence, ctrl: pressed.ctrl, shift: pressed.shift, meta: pressed.meta }
    const previous = this.keyQueues.get(panelKey) ?? Promise.resolve()
    const next = previous.then(async () => {
      const entry = this.running.get(extension)
      if (!entry || entry.state !== "running" || !entry.catalog.permissions.includes("tui.keys")) return
      const surface = { kind: "panel" as const, id }
      await this.handled(entry, "tui/key", await this.call(entry, "tui/key", { surface, key }), { surface, extension })
    }).finally(() => {
      if (this.keyQueues.get(panelKey) === next) this.keyQueues.delete(panelKey)
    })
    this.keyQueues.set(panelKey, next)
    return next
  }

  /** End keyboard capture (Ctrl+C while a panel holds the keyboard). */
  release(): void { this.options.release?.() }

  /** Close the overlay showing `panelKey` (Ctrl+C in the overlay). */
  close(panelKey: string): void { this.options.close?.(panelKey) }

  /**
   * Apply what an action, key, or command-result handler answered: its
   * notice, a re-render, then its commands in order. Malformed commands and
   * commands outside the extension's permissions are dropped with a warning.
   */
  private async handled(entry: Running, method: string, value: unknown, origin: CommandOrigin): Promise<void> {
    if (typeof value !== "object" || value === null) return
    const notice = "notice" in value ? safeText(value.notice) : undefined
    if (notice) this.options.notice?.(notice.slice(0, 200))
    if ("invalidate" in value && value.invalidate === true) this.invalidate(entry)
    const commands = "commands" in value && Array.isArray(value.commands) ? value.commands : []
    for (const raw of commands) {
      const command = validateHostCommand(raw)
      if (typeof command === "string") { this.warn(entry, `${method}: ${command}`); continue }
      const permission = commandPermission(command)
      if (permission && !entry.catalog.permissions.includes(permission)) { this.warn(entry, `${method}: command ${command.command} ignored: missing permission`); continue }
      await this.runCommand(entry, command, origin)
    }
  }

  /** Run one command; a command with a token answers with `tui/command_result`, whose handler may answer in turn. */
  private async runCommand(entry: Running, command: HostCommand, origin: CommandOrigin): Promise<void> {
    const panelKey = `${origin.extension}#${origin.surface.id}`
    if (command.command === "ui.release") return this.options.release?.()
    if (command.command === "ui.open") return this.options.open?.(panelKey)
    if (command.command === "ui.close") return this.options.close?.(panelKey)
    let result: CommandResult | undefined
    try {
      const value = await this.options.executeCommand?.(command, origin)
      if ("token" in command && command.token) result = { token: command.token, ok: true, ...(value === undefined ? {} : { value }) }
    } catch (error) {
      const message = errorLine(error)
      if ("token" in command && command.token) result = { token: command.token, ok: false, error: message }
      else this.options.notice?.(message)
    }
    if (!result || entry.state !== "running") return
    // What the command changed (a created Project, a new session) reaches the extension before its result does.
    await this.pushContext(entry, new Map())
    await this.handled(entry, "tui/command_result", await this.call(entry, "tui/command_result", { surface: origin.surface, result }), origin)
  }

  // Internals --------------------------------------------------------------

  private allowed(id: string, options: LoadOptions): boolean {
    const entry = this.running.get(id)
    const preference = options.enabled(id)
    return options.remote ? entry?.catalog.firstParty === true || preference === true : preference !== false
  }

  private trusted(catalog: CatalogExtension, options: LoadOptions): boolean {
    return options.trusted(catalog.bundleId) ?? (catalog.firstParty && !options.remote)
  }

  private sortRunning(): void {
    const sorted = [...this.running.entries()].sort(([a], [b]) => a.localeCompare(b))
    this.running.clear()
    for (const [id, entry] of sorted) this.running.set(id, entry)
  }

  private bumpAll(): void {
    this.bumpList(); this.bumpSurfaces(); this.bumpHooks(); this.bumpFormat()
  }

  /** Mark every rendered surface of `entry` stale; only views that show its contributions re-render. */
  private invalidate(entry: Running): void {
    entry.generation += 1
    const contributions = entry.contributions
    if (contributions?.panels.length || contributions?.statusItems.length) this.bumpSurfaces()
    if (contributions?.renderers.length) this.bumpHooks()
    if (contributions?.formatters.length) this.bumpFormat()
  }

  /** Contributions of one kind across running extensions, ascending (priority, extension id). */
  private ordered<T extends { readonly priority: number }>(pick: (contributions: Contributions) => readonly T[]): { entry: Running; item: T }[] {
    const out: { entry: Running; item: T }[] = []
    for (const entry of this.running.values()) {
      if (entry.state === "running" && entry.contributions) for (const item of pick(entry.contributions)) out.push({ entry, item })
    }
    return out.sort((a, b) => a.item.priority - b.item.priority || a.entry.catalog.bundleId.localeCompare(b.entry.catalog.bundleId))
  }

  private hookTrees(target: RendererContribution["target"], inputKey: string, width: number, input: ToolCallInput | undefined): HookTrees {
    const renderers = this.ordered((contributions) => contributions.renderers.filter((renderer) => renderer.target === target))
    if (renderers.length === 0) return { decorators: [] }
    const replacer = renderers.filter(({ item }) => item.mode === "replace").at(-1)
    const decorators: HookTree[] = []
    let replace: HookTree | undefined
    for (const { entry, item } of renderers) {
      if (item.mode === "replace" && item !== replacer?.item) continue
      const slot = `renderer:${item.id}:${inputKey}`
      const cacheKey = String(entry.generation)
      const cached = entry.surfaces.get(slot)
      if (cached?.key !== cacheKey && !entry.inflight.has(slot)) {
        entry.inflight.add(slot)
        const scoped = input && !entry.catalog.permissions.includes("tui.transcript.read") ? { id: input.id, tool: input.tool, status: input.status, summary: input.summary } : input
        void this.renderSurface(entry, { kind: "renderer", id: item.id }, width, 0, scoped as JsonValue | undefined, item.mode === "decorate" ? "require" : "forbid").then((rendered) => {
          entry.inflight.delete(slot)
          // The composer is never re-parented (its input keeps focus): its decoration must be a column with the slot as a direct child.
          const composerShape = target !== "composer" || !rendered.node || (rendered.node.kind === "column" && rendered.node.children.some((child) => child.kind === "slot"))
          const view = composerShape ? rendered : { error: `${entry.catalog.bundleId}: a composer decoration must be a column with { kind: "slot" } as a direct child` }
          if (view.error) this.warn(entry, view.error)
          if (entry.surfaces.size >= maxCached) entry.surfaces.clear()
          entry.surfaces.set(slot, { key: cacheKey, view })
          this.bumpHooks()
        })
      }
      const node = cached?.view.node
      if (!node) continue
      const tree = { node, extension: entry.catalog.bundleId, renderer: item.id }
      if (item.mode === "replace") replace = tree
      else decorators.push(tree)
    }
    return { ...(replace ? { replace } : {}), decorators }
  }

  private async renderSurface(entry: Running, surface: { kind: "panel" | "status" | "renderer"; id: string }, width: number, height: number, input: JsonValue | undefined, slot: "forbid" | "require"): Promise<SurfaceView> {
    const result = await this.call<{ root?: unknown }>(entry, "tui/render", { surface, width, height, now: Date.now(), ...(input === undefined ? {} : { input }) })
    if (!result || result.root === null || result.root === undefined) return {}
    const node = validateRenderNode(result.root, { slot, actions: entry.catalog.permissions.includes("tui.action") })
    return typeof node === "string" ? { error: `${entry.catalog.bundleId}: ${node}` } : { node }
  }

  /** One request; failures resolve `undefined` (the caller falls back) and are recorded as warnings. A timeout fails the extension. */
  private async call<T>(entry: Running, method: string, params: unknown): Promise<T | undefined> {
    const host = this.host
    if (!host || entry.state !== "running") return undefined
    try {
      return await this.extRequest<T>(host, entry.catalog.bundleId, method, params)
    } catch (error) {
      if (error instanceof HostTimeoutError) this.timedOut(entry, error.message)
      else this.warn(entry, `${method}: ${error instanceof Error ? error.message : String(error)}`)
      return undefined
    }
  }

  /**
   * One extension request through the host (`ext/call`); its error response
   * rejects. A handler past the VM deadline rejects like a timeout: the
   * extension restarts within its budget, so a runaway cannot keep the VM
   * thread every other extension shares.
   */
  private async extRequest<T>(host: HostChannel, ext: string, method: string, params: unknown): Promise<T> {
    const answer = await host.request<{ response?: { result?: unknown; error?: { code?: unknown; message?: unknown } } }>("ext/call", { ext, request: { jsonrpc: "2.0", id: ++this.requestId, method, params } })
    const response = answer?.response
    if (!response) throw new Error("the extension host gave no response")
    if (response.error) {
      const message = typeof response.error.message === "string" ? response.error.message : "extension error"
      throw response.error.code === extensionStoppedCode ? new HostTimeoutError(message) : new Error(message)
    }
    return response.result as T
  }

  private warn(entry: Running, text: string): void {
    entry.warnings.push(text)
    if (entry.warnings.length > 10) entry.warnings.splice(0, entry.warnings.length - 10)
    this.bumpList()
  }

  private async pushContext(entry: Running, scoped: Map<string, { context: ExtensionContext; json: string }>): Promise<void> {
    if (entry.state !== "running") return
    const key = [...entry.catalog.permissions].sort().join(" ")
    let view = scoped.get(key)
    if (!view) {
      const context = scopeContext(this.context, entry.catalog.permissions)
      view = { context, json: JSON.stringify(context) }
      scoped.set(key, view)
    }
    if (view.json === entry.sentContext) return
    entry.sentContext = view.json
    const result = await this.call<{ invalidate?: unknown }>(entry, "tui/context", { context: view.context })
    if (result?.invalidate !== false) this.invalidate(entry)
  }

  private fail(entry: Running, reason: string): void {
    entry.state = "failed"
    entry.reason = reason
    entry.contributions = undefined
    entry.surfaces.clear()
    for (const timer of entry.timers) clearInterval(timer)
    entry.timers = []
    this.files.release(entry.catalog.bundleId)
    this.bumpAll()
  }

  /**
   * The shared host, started on first use: planned once for the OS sandbox
   * (readable: the SDK and the extension cache, so later extensions need no
   * restart), then spawned.
   */
  private ensureHost(sdk: InstalledSdk): Promise<HostChannel> {
    if (this.host && !this.host.failure) return Promise.resolve(this.host)
    this.hostStart ??= (async () => {
      const bun = this.options.bun ?? globalThis.process.execPath
      // The launcher adds system libraries and the bun install to what the host may read.
      // It resolves every readable path, so the cache must exist before the first extension is written there.
      await mkdir(this.options.cacheRoot, { recursive: true })
      const plan = await planSandbox({ argv: [bun, join(sdk.dir, "src", "main.ts")], policy: this.options.sandbox ?? "best-effort", readable: [sdk.dir, this.options.cacheRoot], cacheRoot: this.options.cacheRoot })
      this.hostIsolated = plan.isolated
      this.hostWarning = plan.warning
      const channel: HostChannel = new HostChannel({
        argv: plan.argv,
        timeoutMs: this.options.timeoutMs ?? 5_000,
        ...(this.options.spawn ? { spawn: this.options.spawn } : {}),
        serve: (method, params) => this.serveHost(method, params),
        log: (ext, line) => this.hostLog(ext, line),
        onExit: (reason) => this.hostExited(channel, reason),
      })
      this.host = channel
      return channel
    })().finally(() => { this.hostStart = undefined })
    return this.hostStart
  }

  private async start(entry: Running): Promise<void> {
    const { catalog } = entry
    const ext = catalog.bundleId
    const now = Date.now()
    const window = this.options.restartWindowMs ?? 60_000
    entry.starts = entry.starts.filter((time) => now - time < window)
    if (entry.starts.length >= (this.options.restartBudget ?? 3)) return this.fail(entry, `restart budget exhausted (${entry.starts.length} starts in ${window / 1000}s)`)
    entry.starts.push(now)
    entry.state = "starting"
    entry.reason = undefined
    entry.warnings = []
    const attempt = entry.starts.length
    const ready = Promise.withResolvers<void>()
    entry.ready = ready.promise
    this.bumpList()
    const sdk = this.options.sdk
    if (!sdk) return this.fail(entry, "the TUI extension SDK is not installed next to this TUI")
    if (!sdkCompatible(catalog.sdk, sdk.version)) return this.fail(entry, `needs SDK ${catalog.sdk}; this TUI ships SDK ${sdk.version}`)
    // Shut down or restarted meanwhile: this start no longer owns the entry.
    const superseded = () => this.running.get(ext) !== entry || entry.state !== "starting" || entry.starts.length !== attempt
    let host: HostChannel | undefined
    try {
      const [dir, started] = await Promise.all([materialize(catalog, this.options.cacheRoot), this.ensureHost(sdk)])
      host = started
      entry.isolated = this.hostIsolated
      if (this.hostWarning) entry.warnings.push(this.hostWarning)
      await host.request("host/load", { ext, entry: join(dir, catalog.entry), permissions: catalog.permissions, jit: entry.jit })
      await this.extRequest(host, ext, "tui/initialize", { api_version: extensionApiVersion, sdk_version: sdk.version, extension_id: ext, permissions: catalog.permissions })
      const context = scopeContext(this.context, catalog.permissions)
      entry.sentContext = JSON.stringify(context)
      const activated = await this.extRequest<{ contributions?: unknown }>(host, ext, "tui/activate", { context })
      const checked = validateContributions(activated?.contributions, catalog.permissions)
      if (typeof checked === "string") throw new Error(checked)
      if (superseded()) return
      entry.contributions = checked.contributions
      entry.warnings.push(...checked.warnings)
      entry.state = "running"
      entry.generation += 1
      for (const panel of checked.contributions.panels) {
        if (panel.refreshMs) entry.timers.push(setInterval(() => this.invalidate(entry), panel.refreshMs))
      }
      this.bumpAll()
    } catch (error) {
      if (superseded()) return
      const log = entry.log.at(-1)
      const reason = `${error instanceof Error ? error.message : String(error)}${log ? ` · ${log}` : ""}`
      this.fail(entry, reason)
      if (host && !host.failure) await host.request("host/unload", { ext }).catch(() => undefined)
      this.options.notice?.(`Extension ${ext} failed: ${reason}`)
    } finally {
      ready.resolve()
    }
  }

  /** A running extension stopped answering: fail it, drop its VM, and restart it within its budget. */
  private timedOut(entry: Running, reason: string): void {
    if (entry.state !== "running") return
    this.fail(entry, reason)
    void this.host?.request("host/unload", { ext: entry.catalog.bundleId }).catch(() => undefined)
    this.restartLater(entry)
  }

  /** The host died: every extension in it failed; the running ones restart (in a new host) within their budget. */
  private hostExited(channel: HostChannel, reason: string): void {
    if (this.host !== channel) return
    this.host = undefined
    for (const entry of this.running.values()) {
      if (entry.state !== "running") continue
      const log = entry.log.at(-1)
      this.fail(entry, `extension host ${reason}${log ? ` · ${log}` : ""}`)
      this.restartLater(entry)
    }
  }

  private restartLater(entry: Running): void {
    if (this.running.get(entry.catalog.bundleId) !== entry) return
    entry.restartTimer = setTimeout(() => {
      entry.restartTimer = undefined
      if (this.running.get(entry.catalog.bundleId) === entry && entry.state === "failed") void this.start(entry)
    }, 1_000)
  }

  private hostLog(ext: string | undefined, line: string): void {
    const entry = ext === undefined ? undefined : this.running.get(ext)
    // The host's own lines (a crash, a sandbox refusal) belong to every extension it runs.
    for (const target of entry ? [entry] : this.running.values()) {
      target.log.push(line.slice(0, 1_000))
      if (target.log.length > 20) target.log.splice(0, target.log.length - 20)
    }
    this.bumpList()
  }

  /** The host's requests: an extension's `api.fs`, checked against its permission here (the VM's own check is not trusted). */
  private async serveHost(method: string, params: Readonly<Record<string, unknown>>): Promise<unknown> {
    const ext = typeof params.ext === "string" ? params.ext : ""
    const entry = this.running.get(ext)
    if (!entry || (entry.state !== "running" && entry.state !== "starting")) throw new Error("extension not running")
    if (!entry.catalog.permissions.includes("fs.read")) throw new Error("missing permission fs.read")
    const path = typeof params.path === "string" ? params.path : ""
    switch (method) {
      case "fs/read": return await this.files.read(path)
      case "fs/list": return await this.files.list(path)
      case "fs/stat": return await this.files.stat(path)
      case "fs/watch": {
        const watch = Number(params.watch)
        try {
          return await this.files.watch(ext, watch, path, params.recursive === true)
        } catch (error) {
          // The SDK waits for this `closed` event to end the watch (it may arrive while the extension still activates).
          void this.fsEvent(ext, watch, [{ path, kind: "closed" }])
          throw error
        }
      }
      case "fs/unwatch": return this.files.unwatch(ext, Number(params.watch))
      default: throw new Error(`unknown host request ${method}`)
    }
  }

  /** A batch of watch events: the extension's handler may ask for a redraw or a notice (commands need a surface and are ignored). */
  private async fsEvent(ext: string, watch: number, events: readonly FsEvent[]): Promise<void> {
    const entry = this.running.get(ext)
    if (entry?.state === "starting") await entry.ready
    if (!entry || entry.state !== "running") return
    const result = await this.call<{ invalidate?: unknown; notice?: unknown }>(entry, "tui/fs_event", { watch, events })
    const notice = safeText(result?.notice)
    if (notice) this.options.notice?.(notice.slice(0, 200))
    if (result?.invalidate === true) this.invalidate(entry)
  }

  private async shutdown(entry: Running): Promise<void> {
    clearTimeout(entry.restartTimer)
    for (const timer of entry.timers) clearInterval(timer)
    entry.timers = []
    const wasLoaded = entry.state === "running" || entry.state === "starting"
    entry.contributions = undefined
    entry.surfaces.clear()
    entry.state = "disabled"
    this.files.release(entry.catalog.bundleId)
    const host = this.host
    if (!wasLoaded || !host || host.failure) return
    await this.extRequest(host, entry.catalog.bundleId, "tui/shutdown", {}).catch(() => undefined)
    await host.request("host/unload", { ext: entry.catalog.bundleId }).catch(() => undefined)
  }
}

function validateHostCommand(value: unknown): HostCommand | string {
  if (!value || typeof value !== "object" || Array.isArray(value)) return "malformed command"
  const row = value as Record<string, unknown>
  if (typeof row.command !== "string") return "malformed command"
  const command = row.command
  if (!["session.open", "session.menu", "session.new_temporary", "project.switch", "project.menu", "project.create", "project.rename", "project.set_roots", "project.delete", "fs.complete", "ui.release", "ui.close", "ui.open"].includes(command)) return `unknown command ${command}`
  if (["session.open", "session.menu", "project.switch", "project.menu", "project.rename", "project.set_roots", "project.delete"].includes(command) && typeof row.id !== "string") return `${command} requires id`
  if (["project.create", "project.rename"].includes(command) && typeof row.name !== "string") return `${command} requires name`
  if (["project.create", "project.set_roots"].includes(command) && (!Array.isArray(row.roots) || row.roots.some((root) => typeof root !== "string"))) return `${command} requires roots`
  if (command === "fs.complete" && (typeof row.input !== "string" || typeof row.token !== "string")) return "fs.complete requires input and token"
  return value as HostCommand
}

function commandPermission(command: HostCommand): ExtensionPermission | undefined {
  if (command.command.startsWith("session.")) return "tui.session.control"
  if (command.command.startsWith("project.") || command.command === "fs.complete") return "tui.project.control"
  return undefined
}

function splitKey(key: string): [string, string] {
  const at = key.lastIndexOf("#")
  return at < 0 ? [key, ""] : [key.slice(0, at), key.slice(at + 1)]
}

/** The TUI's one manager; app/run.tsx configures it, views read it. Without a catalog it contributes nothing. */
export const extensionManager = new ExtensionManager({ cacheRoot: "", sdk: locateSdk() })
