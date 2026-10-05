import {
  REPLACEABLE_SURFACES, TUI_EXTENSION_API_VERSION, TUI_EXTENSION_SDK_VERSION,
  type CommandResult, type Contributions, type ExtensionAction, type ExtensionContext, type FormatInput, type HandlerResult, type InterceptDecision,
  type JsonValue, type KeyEvent, type PanelPlacement, type ReplaceableSurface, type RpcEnvelope, type SurfaceRef, type ToolCallInput, type TuiResponse,
} from "./protocol";
import type { RenderNode } from "./render";

export interface FsEvent { readonly path: string; readonly kind: "change" | "rename" | "closed" }
export interface FsEntry { readonly name: string; readonly kind: "file" | "dir" | "other" }
export interface FsStat { readonly kind: "file" | "dir" | "other"; readonly size: number; readonly mtime: number }
export interface TuiFsApi {
  readonly read: (path: string) => Promise<string>;
  readonly list: (path: string) => Promise<readonly FsEntry[]>;
  readonly stat: (path: string) => Promise<FsStat | null>;
  readonly watch: (path: string, handler: (events: readonly FsEvent[]) => MaybePromise<HandlerResult>, options?: { readonly recursive?: boolean }) => () => void;
}

/** What a render callback sees: the latest host context and the surface size in cells. */
export interface RenderContext {
  readonly context: ExtensionContext;
  readonly width: number;
  readonly height: number;
  /** Host-supplied epoch milliseconds; 0 when the host does not supply a clock. */
  readonly now: number;
}

/** A string renders as a plain text node; `null`/`undefined` hides the surface (or keeps the built-in rendering). */
export type Renderable = RenderNode | string | null | undefined;
type MaybePromise<T> = T | Promise<T>;
/** Return a notice, host commands, or an explicit invalidation decision. */
export type ActionHandler = (action: ExtensionAction, context: ExtensionContext) => MaybePromise<HandlerResult>;

export interface PanelOptions {
  readonly id: string;
  readonly title: string;
  /** `sidebar` (default): shown automatically in the extension column; `pane`: only where `/layout` places it. */
  readonly placement?: PanelPlacement;
  /** Render this panel instead of a built-in pane or surface. */
  readonly replaces?: ReplaceableSurface;
  /** Re-render at most this often (milliseconds, at least 1000) in addition to context changes. */
  readonly refreshMs?: number;
  readonly render: (context: RenderContext) => MaybePromise<Renderable>;
  readonly onAction?: ActionHandler;
  /** Capture host key events (requires `tui.keys`). */
  readonly onKey?: (key: KeyEvent, context: ExtensionContext) => MaybePromise<HandlerResult>;
  readonly onResult?: (result: CommandResult, context: ExtensionContext) => MaybePromise<HandlerResult>;
}

export interface StatusItemOptions {
  readonly id: string;
  /** The label in the Context box (`Branch`, `Mode`, …). */
  readonly label: string;
  /** Drop order on a narrow status line, 1 (kept longest) to 9 (dropped first). Default 7. */
  readonly priority?: number;
  readonly render: (context: RenderContext) => MaybePromise<string | null | undefined>;
}

interface RendererBase {
  readonly id: string;
  /** `replace`: the highest priority replaces the built-in; `decorate`: wraps it through one `{ kind: "slot" }`. */
  readonly mode?: "replace" | "decorate";
  /** Higher runs later (replace: wins; decorate: outermost). Default 0. */
  readonly priority?: number;
  readonly onAction?: ActionHandler;
}
export type RendererOptions =
  | RendererBase & { readonly target: "tool_call"; readonly render: (input: ToolCallInput, context: RenderContext) => MaybePromise<Renderable> }
  /** The composer is decorate-only: its input and keys always stay the host's. */
  | RendererBase & { readonly target: "composer"; readonly mode?: "decorate"; readonly render: (input: undefined, context: RenderContext) => MaybePromise<Renderable> };

export interface FormatterOptions {
  readonly id: string;
  readonly priority?: number;
  /** Tool output pretty printing; `undefined` keeps `input.text`. */
  readonly format: (input: FormatInput, context: RenderContext) => MaybePromise<string | null | undefined>;
}

export interface InterceptorOptions {
  readonly id: string;
  /** `submit`: a prompt the user sends (not commands or `!` shell lines). */
  readonly target: "submit";
  /** Higher runs first. Default 0. */
  readonly priority?: number;
  readonly intercept: (input: { readonly text: string }, context: ExtensionContext) => MaybePromise<InterceptDecision | undefined>;
}

export interface TuiExtensionApi {
  readonly registerPanel: (panel: PanelOptions) => void;
  readonly registerStatusItem: (item: StatusItemOptions) => void;
  readonly registerRenderer: (renderer: RendererOptions) => void;
  readonly registerFormatter: (formatter: FormatterOptions) => void;
  readonly registerInterceptor: (interceptor: InterceptorOptions) => void;
  readonly onContext: (handler: (context: ExtensionContext) => MaybePromise<boolean | void>) => void;
  readonly fs: TuiFsApi;
}

export interface TuiExtension {
  /** Register contributions. Registration is only possible while `activate` runs. */
  readonly activate: (api: TuiExtensionApi, context: ExtensionContext) => MaybePromise<void>;
  readonly deactivate?: () => MaybePromise<void>;
}

/** Typed identity: `export default defineTuiExtension({ activate(api) { … } })`. */
export function defineTuiExtension(extension: TuiExtension): TuiExtension {
  return extension;
}

const error = (id: string | number | null, code: number, message: string): TuiResponse => ({ jsonrpc: "2.0", id, error: { code, message } });
const result = (id: string | number, value: unknown): RpcEnvelope => ({ jsonrpc: "2.0", id, result: value as JsonValue });
const idPattern = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const toNode = (value: Renderable): RenderNode | null => typeof value === "string" ? { kind: "text", text: value } : value ?? null;

/** Dispatches host requests to one extension. It contains no host UI dependencies. */
export class TuiExtensionRunner {
  private readonly panels = new Map<string, PanelOptions>();
  private readonly statuses = new Map<string, StatusItemOptions>();
  private readonly renderers = new Map<string, RendererOptions>();
  private readonly formatters = new Map<string, FormatterOptions>();
  private readonly interceptors = new Map<string, InterceptorOptions>();
  private readonly contextHandlers: ((context: ExtensionContext) => MaybePromise<boolean | void>)[] = [];
  private readonly watches = new Map<number, (events: readonly FsEvent[]) => MaybePromise<HandlerResult>>();
  private context: ExtensionContext = { terminal: { columns: 0, rows: 0 } };
  private initialized = false;
  private permissions: readonly string[] = [];
  private activated = false;
  private registering = false;
  private nextWatch = 1;
  constructor(private readonly extension: TuiExtension, private readonly hostCall?: (method: string, params: Record<string, unknown>) => Promise<unknown>) {}

  async handle(frame: RpcEnvelope): Promise<RpcEnvelope | TuiResponse> {
    if (!("method" in frame)) return error(frame.id, -32600, "request required");
    const params = (frame.params ?? {}) as Record<string, unknown>;
    if (frame.method === "tui/initialize") {
      if (this.initialized) return error(frame.id, -32600, "already initialized");
      if (params.api_version !== TUI_EXTENSION_API_VERSION) return error(frame.id, -32602, `unsupported api_version ${String(params.api_version)}`);
      this.initialized = true;
      this.permissions = Array.isArray(params.permissions) ? params.permissions.filter((permission): permission is string => typeof permission === "string") : [];
      return result(frame.id, { api_version: TUI_EXTENSION_API_VERSION, sdk_version: TUI_EXTENSION_SDK_VERSION });
    }
    if (!this.initialized) return error(frame.id, -32001, "initialize is required first");
    try {
      switch (frame.method) {
        case "tui/activate": {
          if (this.activated) return error(frame.id, -32600, "already activated");
          this.context = params.context as unknown as ExtensionContext;
          this.registering = true;
          try { await this.extension.activate(this.api(), this.context); } finally { this.registering = false; }
          this.activated = true;
          return result(frame.id, { contributions: this.contributions() });
        }
        case "tui/render": {
          const surface = params.surface as unknown as SurfaceRef;
          const size = { context: this.context, width: Number(params.width), height: Number(params.height), now: typeof params.now === "number" ? params.now : 0 };
          if (surface.kind === "panel") return result(frame.id, { root: toNode(await this.lookup(this.panels, surface.id).render(size)) });
          if (surface.kind === "status") {
            const text = await this.lookup(this.statuses, surface.id).render(size);
            return result(frame.id, { root: text ? { kind: "text", text } : null });
          }
          const renderer = this.lookup(this.renderers, surface.id);
          return result(frame.id, { root: toNode(await renderer.render(params.input as never, size)) });
        }
        case "tui/format": {
          const text = await this.lookup(this.formatters, String(params.formatter)).format(params.input as unknown as FormatInput, { context: this.context, width: Number(params.width), height: 0, now: 0 });
          return result(frame.id, { text: text ?? null });
        }
        case "tui/intercept": {
          const decision = await this.lookup(this.interceptors, String(params.interceptor)).intercept(params.input as { text: string }, this.context);
          return result(frame.id, decision ?? { decision: "continue" });
        }
        case "tui/context": {
          this.context = params.context as unknown as ExtensionContext;
          let invalidate = this.contextHandlers.length === 0;
          for (const handler of this.contextHandlers) if ((await handler(this.context)) !== false) invalidate = true;
          return result(frame.id, { invalidate });
        }
        case "tui/action":
        case "tui/key":
        case "tui/command_result": {
          const surface = params.surface as unknown as SurfaceRef;
          const panel = surface.kind === "panel" ? this.panels.get(surface.id) : undefined;
          let handled: HandlerResult;
          if (frame.method === "tui/action") {
            const owner = panel ?? (surface.kind === "renderer" ? this.renderers.get(surface.id) : undefined);
            if (!owner?.onAction) return result(frame.id, { invalidate: false });
            handled = await owner.onAction(params.action as unknown as ExtensionAction, this.context);
          } else if (frame.method === "tui/key") {
            if (!panel?.onKey) return result(frame.id, { invalidate: false });
            handled = await panel.onKey(params.key as unknown as KeyEvent, this.context);
          } else {
            if (!panel?.onResult) return result(frame.id, { invalidate: false });
            handled = await panel.onResult(params.result as unknown as CommandResult, this.context);
          }
          return result(frame.id, typeof handled === "string"
            ? { invalidate: true, notice: handled }
            : { ...handled, invalidate: handled?.invalidate ?? true });
        }
        case "tui/fs_event": {
          const watch = Number(params.watch);
          const handler = this.watches.get(watch);
          if (!handler) return result(frame.id, { invalidate: false });
          const events = Array.isArray(params.events) ? params.events as FsEvent[] : [];
          // `closed` ends the watch (the host refused it, or its Project closed).
          if (events.some((event) => event.kind === "closed")) this.watches.delete(watch);
          const handled = await handler(events);
          return result(frame.id, typeof handled === "string" ? { invalidate: true, notice: handled } : { ...handled, invalidate: handled?.invalidate ?? true });
        }
        case "tui/shutdown":
          await this.extension.deactivate?.();
          return result(frame.id, null);
        default: return error(frame.id, -32601, `method not found: ${frame.method}`);
      }
    } catch (caught) {
      return error(frame.id, -32000, caught instanceof Error ? caught.message : "extension handler failed");
    }
  }

  private lookup<T>(map: Map<string, T>, id: string): T {
    const found = map.get(id);
    if (!found) throw new Error(`unknown surface ${id}`);
    return found;
  }

  private contributions(): Contributions {
    return {
      panels: [...this.panels.values()].map((panel) => ({
        id: panel.id, title: panel.title, placement: panel.placement ?? "sidebar",
        ...(panel.replaces ? { replaces: panel.replaces } : {}),
        ...(panel.refreshMs ? { refresh_ms: panel.refreshMs } : {}),
        ...(panel.onKey ? { keys: true as const } : {}),
      })),
      status_items: [...this.statuses.values()].map((item) => ({ id: item.id, label: item.label, priority: item.priority ?? 7 })),
      renderers: [...this.renderers.values()].map((renderer) => ({ id: renderer.id, target: renderer.target, mode: renderer.mode ?? "decorate", priority: renderer.priority ?? 0 })),
      formatters: [...this.formatters.values()].map((formatter) => ({ id: formatter.id, priority: formatter.priority ?? 0 })),
      interceptors: [...this.interceptors.values()].map((interceptor) => ({ id: interceptor.id, target: interceptor.target, priority: interceptor.priority ?? 0 })),
    };
  }

  private api(): TuiExtensionApi {
    const add = <T extends { readonly id: string; readonly priority?: number }>(map: Map<string, T>, kind: string, value: T): void => {
      if (!this.registering) throw new Error(`${kind} must be registered while activate runs`);
      if (!idPattern.test(value.id)) throw new Error(`invalid ${kind} id ${JSON.stringify(value.id)} (lowercase letters, digits, . _ -)`);
      if (map.has(value.id)) throw new Error(`duplicate ${kind} id ${value.id}`);
      if (value.priority !== undefined && !Number.isSafeInteger(value.priority)) throw new Error(`${kind} ${value.id}: priority must be an integer`);
      map.set(value.id, value);
    };
    /** The host (the TUI) answers in these shapes; it also enforces the permission and the Project roots. */
    const fsCall = <T>(method: string, params: Record<string, unknown>): Promise<T> => {
      if (!this.permissions.includes("fs.read")) return Promise.reject(new Error("missing permission fs.read"));
      if (!this.hostCall) return Promise.reject(new Error("filesystem host unavailable"));
      return this.hostCall(method, params) as Promise<T>;
    };
    return {
      registerPanel: (panel) => {
        if (panel.replaces !== undefined && !REPLACEABLE_SURFACES.includes(panel.replaces)) throw new Error(`panel ${panel.id}: cannot replace ${panel.replaces}`);
        if (panel.refreshMs !== undefined && (!Number.isSafeInteger(panel.refreshMs) || panel.refreshMs < 1000)) throw new Error(`panel ${panel.id}: refreshMs must be an integer >= 1000`);
        add(this.panels, "panel", panel);
      },
      registerStatusItem: (item) => {
        if (item.priority !== undefined && (item.priority < 1 || item.priority > 9)) throw new Error(`status item ${item.id}: priority must be 1..9`);
        add(this.statuses, "status item", item);
      },
      registerRenderer: (renderer) => {
        const mode: string | undefined = renderer.mode;
        if (renderer.target === "composer" && mode === "replace") throw new Error(`renderer ${renderer.id}: the composer can only be decorated`);
        add(this.renderers, "renderer", renderer);
      },
      registerFormatter: (formatter) => add(this.formatters, "formatter", formatter),
      registerInterceptor: (interceptor) => add(this.interceptors, "interceptor", interceptor),
      onContext: (handler) => {
        if (!this.registering) throw new Error("onContext must be registered while activate runs");
        this.contextHandlers.push(handler);
      },
      fs: {
        read: (path) => fsCall<{ text: string }>("fs/read", { path }).then((value) => value.text),
        list: (path) => fsCall<{ entries: readonly FsEntry[] }>("fs/list", { path }).then((value) => value.entries),
        stat: (path) => fsCall<FsStat | null>("fs/stat", { path }),
        watch: (path, handler, options = {}) => {
          const watch = this.nextWatch++;
          this.watches.set(watch, handler);
          // A watch the host refuses (outside the roots, over the limit) ends with a `closed` event from the host.
          fsCall<null>("fs/watch", { watch, path, recursive: options.recursive ?? false }).catch(() => undefined);
          return () => {
            if (this.watches.delete(watch)) void fsCall<null>("fs/unwatch", { watch }).catch(() => undefined);
          };
        },
      },
    };
  }
}
