import {
  BUNDLE_API_METHODS, PANE_SLOTS, REPLACEABLE_SURFACES, TUI_EXTENSION_API_VERSION, TUI_EXTENSION_SDK_VERSION,
  checkBundleConfinement, isValidSessionId, validateBundleApiPath,
  type BundleApiMethod, type CommandResult, type Contributions, type ExtensionAction, type ExtensionContext,
  type FormatInput, type HandlerResult, type InterceptDecision, type JsonValue, type KeyEvent, type PaneSize,
  type PaneSlot, type PanelPlacement, type ReplaceableSurface, type RpcEnvelope, type SurfaceRef,
  type ToolCallInput, type TuiResponse,
} from "./protocol";
import type { RenderNode } from "./render";
import type { FsEvent, ToolCardPresentation } from "./wire";

export type { FsEvent } from "./wire";
export interface FsEntry { readonly name: string; readonly kind: "file" | "dir" | "other" }
export interface FsStat { readonly kind: "file" | "dir" | "other"; readonly size: number; readonly mtime: number }
export interface TuiFsApi {
  readonly read: (path: string) => Promise<string>;
  readonly list: (path: string) => Promise<readonly FsEntry[]>;
  readonly stat: (path: string) => Promise<FsStat | null>;
  readonly watch: (path: string, handler: (events: readonly FsEvent[]) => MaybePromise<HandlerResult>, options?: { readonly recursive?: boolean }) => () => void;
}

/** Options for calling a bundle-scoped backend HTTP endpoint (`api.bundleApi`). */
export interface BundleApiRequestOptions {
  readonly method: BundleApiMethod;
  /** Path below `/v2/bundles/<bundleId>/api` (must start with `/`, no `..`). */
  readonly path: string;
  readonly body?: JsonValue;
  readonly query?: Readonly<Record<string, string>>;
  /** Optional session ID for session-scoped `/v2/sessions/<sessionId>/bundles/<bundleId><path>`. */
  readonly sessionId?: string;
  /** Optional explicit bundle ID; strictly confined to caller's `bundleId` unless explicitly authorized. */
  readonly bundleId?: string;
}

export interface BundleApiCallOptions {
  readonly query?: Readonly<Record<string, string>>;
  readonly sessionId?: string;
  readonly bundleId?: string;
}

/** Async helper for invoking the bundle's own `BundleApi` routes (requires `tui.bundle_api`). */
export interface TuiBundleApi {
  readonly request: <T extends JsonValue = JsonValue>(options: BundleApiRequestOptions) => Promise<T>;
  readonly get: <T extends JsonValue = JsonValue>(path: string, options?: BundleApiCallOptions) => Promise<T>;
  readonly post: <T extends JsonValue = JsonValue>(path: string, body?: JsonValue, options?: BundleApiCallOptions) => Promise<T>;
  readonly put: <T extends JsonValue = JsonValue>(path: string, body?: JsonValue, options?: BundleApiCallOptions) => Promise<T>;
  readonly patch: <T extends JsonValue = JsonValue>(path: string, body?: JsonValue, options?: BundleApiCallOptions) => Promise<T>;
  readonly delete: <T extends JsonValue = JsonValue>(path: string, options?: BundleApiCallOptions) => Promise<T>;
}

/** Helper for reading `Projection::extensions[bundleId]` (requires `tui.extension_state.read`). */
export interface TuiExtensionStateApi {
  /** Synchronously read folded extension state from the latest pushed context (strictly confined to caller's `bundleId`). */
  readonly get: <T extends JsonValue = JsonValue>(bundleId?: string) => T | undefined;
  /** Asynchronously query folded extension state from the host (optionally for another `sessionId`). */
  readonly read: <T extends JsonValue = JsonValue>(options?: { readonly sessionId?: string; readonly bundleId?: string }) => Promise<T | undefined>;
}

/** What a render callback sees: the latest host context and the surface size in cells. */
export interface RenderContext {
  readonly context: ExtensionContext;
  readonly width: number;
  readonly height: number;
  /** Host-supplied epoch milliseconds; 0 when the host does not supply a clock. */
  readonly now: number;
}

/** What a dynamic named pane render callback sees (adds optional instance id and `pane.open` params). */
export interface PaneRenderContext extends RenderContext {
  readonly instance?: string;
  readonly params?: JsonValue;
}

/** A string renders as a plain text node; `null`/`undefined` hides the surface (or keeps the built-in rendering). */
export type Renderable = RenderNode | string | null | undefined;
type MaybePromise<T> = T | Promise<T>;
/** Return a notice, host commands, or an explicit invalidation decision. */
export type ActionHandler = (action: ExtensionAction, context: ExtensionContext, meta?: { readonly instance?: string }) => MaybePromise<HandlerResult>;

export interface PaneSizeOptions {
  readonly width?: number | string;
  readonly height?: number | string;
  readonly minWidth?: number;
  readonly minHeight?: number;
}

export interface PanelOptions {
  readonly id: string;
  readonly title: string;
  /** `sidebar` (default): shown automatically in the extension column; `pane`: only where `/layout` places it. */
  readonly placement?: PanelPlacement;
  /** Render this panel instead of a built-in pane or surface. */
  readonly replaces?: ReplaceableSurface;
  /** Optional default layout slot when placed as a pane. */
  readonly defaultSlot?: PaneSlot;
  /** Optional default size hint when placed as a pane. */
  readonly defaultSize?: PaneSizeOptions;
  /** Re-render at most this often (milliseconds, at least 1000) in addition to context changes. */
  readonly refreshMs?: number;
  readonly render: (context: RenderContext) => MaybePromise<Renderable>;
  readonly onAction?: ActionHandler;
  /** Capture host key events (requires `tui.keys`). */
  readonly onKey?: (key: KeyEvent, context: ExtensionContext, meta?: { readonly instance?: string }) => MaybePromise<HandlerResult>;
  readonly onResult?: (result: CommandResult, context: ExtensionContext, meta?: { readonly instance?: string }) => MaybePromise<HandlerResult>;
}

/** Options for `api.registerPane` (dynamic named panes opened/pinned/closed/focused by host commands). */
export interface PaneOptions {
  readonly id: string;
  readonly title: string;
  /** Preferred layout slot when opened without an explicit override. */
  readonly defaultSlot?: PaneSlot;
  /** Preferred dimensions when opened without an explicit override. */
  readonly defaultSize?: PaneSizeOptions;
  /** Whether the user or extension can pin this pane (default `true`). */
  readonly pinnable?: boolean;
  /** Whether the user or extension can close this pane (default `true`). */
  readonly closable?: boolean;
  /** Re-render at most this often (milliseconds, at least 1000) in addition to context changes. */
  readonly refreshMs?: number;
  readonly render: (context: PaneRenderContext) => MaybePromise<Renderable>;
  readonly onAction?: ActionHandler;
  /** Capture host key events (requires `tui.keys`). */
  readonly onKey?: (key: KeyEvent, context: ExtensionContext, meta?: { readonly instance?: string }) => MaybePromise<HandlerResult>;
  readonly onResult?: (result: CommandResult, context: ExtensionContext, meta?: { readonly instance?: string }) => MaybePromise<HandlerResult>;
}

/** Input received by a registered slash command handler (`tui/command`). */
export interface SlashCommandInvocation {
  readonly name: string;
  readonly args: string;
  readonly raw: string;
}

/** Options for `api.registerCommand` (contributes a slash command to the TUI command palette). */
export interface SlashCommandOptions {
  /** Command name without leading `/`, e.g. `"workflows"` (cannot start with `skill:`). */
  readonly name: string;
  readonly description: string;
  /** Optional argument hint shown in completion UI (e.g. `<id>` or `[filter]`). */
  readonly argumentHint?: string;
  readonly execute: (input: SlashCommandInvocation, context: ExtensionContext) => MaybePromise<HandlerResult>;
  /** Optional handler for tokenized `HostCommand` results emitted by `execute`. */
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
export interface ToolRendererOptions {
  readonly id: string;
  readonly tools: readonly string[];
  readonly priority?: number;
  readonly render: (input: ToolCallInput, context: RenderContext) => MaybePromise<ToolCardPresentation | null | undefined>;
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
  /** Authoritative bundle ID supplied by the host at `tui/initialize`. */
  readonly bundleId: string;
  readonly registerPanel: (panel: PanelOptions) => void;
  readonly registerPane: (pane: PaneOptions) => void;
  readonly registerCommand: (command: SlashCommandOptions) => void;
  readonly registerStatusItem: (item: StatusItemOptions) => void;
  readonly registerToolRenderer: (renderer: ToolRendererOptions) => void;
  readonly registerRenderer: (renderer: RendererOptions) => void;
  readonly registerFormatter: (formatter: FormatterOptions) => void;
  readonly registerInterceptor: (interceptor: InterceptorOptions) => void;
  readonly onContext: (handler: (context: ExtensionContext) => MaybePromise<boolean | void>) => void;
  readonly fs: TuiFsApi;
  readonly bundleApi: TuiBundleApi;
  readonly extensionState: TuiExtensionStateApi;
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
const commandNamePattern = /^[a-z0-9][a-z0-9._:-]{0,63}$/;
const percentPattern = /^([1-9][0-9]?|100)%$/;
const toNode = (value: Renderable): RenderNode | null => typeof value === "string" ? { kind: "text", text: value } : value ?? null;

function normalizePaneSize(id: string, size: PaneSizeOptions | undefined): PaneSize | undefined {
  if (!size) return undefined;
  const checkDim = (name: string, value: number | string | undefined): void => {
    if (value === undefined) return;
    if (typeof value === "number") {
      if (!Number.isSafeInteger(value) || value < 1 || value > 1000) throw new Error(`${id}: ${name} must be an integer 1..1000 or percentage string`);
      return;
    }
    if (typeof value !== "string" || !percentPattern.test(value)) {
      throw new Error(`${id}: ${name} must be an integer 1..1000 or percentage string`);
    }
  };
  const checkMin = (name: string, value: number | undefined): void => {
    if (value !== undefined && (!Number.isSafeInteger(value) || value < 1 || value > 1000)) {
      throw new Error(`${id}: ${name} must be an integer 1..1000`);
    }
  };
  checkDim("width", size.width);
  checkDim("height", size.height);
  checkMin("minWidth", size.minWidth);
  checkMin("minHeight", size.minHeight);
  return {
    ...(size.width !== undefined ? { width: size.width } : {}),
    ...(size.height !== undefined ? { height: size.height } : {}),
    ...(size.minWidth !== undefined ? { min_width: size.minWidth } : {}),
    ...(size.minHeight !== undefined ? { min_height: size.minHeight } : {}),
  };
}

/** Dispatches host requests to one extension. It contains no host UI dependencies. */
export class TuiExtensionRunner {
  private readonly panels = new Map<string, PanelOptions>();
  private readonly panes = new Map<string, PaneOptions>();
  private readonly commands = new Map<string, SlashCommandOptions>();
  private readonly statuses = new Map<string, StatusItemOptions>();
  private readonly renderers = new Map<string, RendererOptions | (ToolRendererOptions & { target: "tool_call"; mode: "replace" })>();
  private readonly formatters = new Map<string, FormatterOptions>();
  private readonly interceptors = new Map<string, InterceptorOptions>();
  private readonly contextHandlers: ((context: ExtensionContext) => MaybePromise<boolean | void>)[] = [];
  private readonly watches = new Map<number, (events: readonly FsEvent[]) => MaybePromise<HandlerResult>>();
  private context: ExtensionContext = { terminal: { columns: 0, rows: 0 } };
  private initialized = false;
  private extensionId = "";
  private permissions: readonly string[] = [];
  private authorizedBundles: readonly string[] = [];
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
      this.extensionId = typeof params.extension_id === "string" ? params.extension_id : "";
      this.permissions = Array.isArray(params.permissions) ? params.permissions.filter((permission): permission is string => typeof permission === "string") : [];
      this.authorizedBundles = Array.isArray(params.authorized_bundles) ? params.authorized_bundles.filter((id): id is string => typeof id === "string") : [];
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
          if (surface.kind === "pane") {
            const pane = this.lookup(this.panes, surface.id);
            const paneContext: PaneRenderContext = {
              ...size,
              ...(surface.instance ? { instance: surface.instance } : {}),
              ...(params.input !== undefined ? { params: params.input as JsonValue } : {}),
            };
            return result(frame.id, { root: toNode(await pane.render(paneContext)) });
          }
          if (surface.kind === "status") {
            const text = await this.lookup(this.statuses, surface.id).render(size);
            return result(frame.id, { root: text ? { kind: "text", text } : null });
          }
          const renderer = this.lookup(this.renderers, surface.id);
          if ("tools" in renderer) {
            const input = params.input as unknown as ToolCallInput;
            const card = renderer.tools.includes(input.tool) ? await renderer.render(input, size) : null;
            return result(frame.id, card ? {
              root: card.body ?? null,
              card: { summary: card.summary, statusChip: card.statusChip, expandedByDefault: card.expandedByDefault },
            } : { root: null });
          }
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
        case "tui/command": {
          const name = String(params.name ?? "");
          const command = this.lookup(this.commands, name);
          const handled = await command.execute({
            name,
            args: typeof params.args === "string" ? params.args : "",
            raw: typeof params.raw === "string" ? params.raw : `/${name}`,
          }, this.context);
          return result(frame.id, this.formatHandlerResult(handled));
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
          const meta = surface.instance ? { instance: surface.instance } : undefined;
          const panel = surface.kind === "panel" ? this.panels.get(surface.id) : undefined;
          const pane = surface.kind === "pane" ? this.panes.get(surface.id) : undefined;
          const interactive = panel ?? pane;
          let handled: HandlerResult;
          if (frame.method === "tui/action") {
            const owner = interactive ?? (surface.kind === "renderer" ? this.renderers.get(surface.id) : undefined);
            if (!owner?.onAction) return result(frame.id, { invalidate: false });
            handled = await owner.onAction(params.action as unknown as ExtensionAction, this.context, meta);
          } else if (frame.method === "tui/key") {
            if (!interactive?.onKey) return result(frame.id, { invalidate: false });
            handled = await interactive.onKey(params.key as unknown as KeyEvent, this.context, meta);
          } else {
            const commandOwner = surface.kind === "command" ? this.commands.get(surface.id) : undefined;
            if (interactive?.onResult) {
              handled = await interactive.onResult(params.result as unknown as CommandResult, this.context, meta);
            } else if (commandOwner?.onResult) {
              handled = await commandOwner.onResult(params.result as unknown as CommandResult, this.context);
            } else {
              return result(frame.id, { invalidate: false });
            }
          }
          return result(frame.id, this.formatHandlerResult(handled));
        }
        case "tui/fs_event": {
          const watch = Number(params.watch);
          const handler = this.watches.get(watch);
          if (!handler) return result(frame.id, { invalidate: false });
          const events = Array.isArray(params.events) ? params.events as FsEvent[] : [];
          // `closed` ends the watch (the host refused it, or its Project closed).
          if (events.some((event) => event.kind === "closed")) this.watches.delete(watch);
          const handled = await handler(events);
          return result(frame.id, this.formatHandlerResult(handled));
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

  private formatHandlerResult(handled: HandlerResult): { readonly invalidate: boolean; readonly notice?: string; readonly commands?: readonly unknown[] } {
    return typeof handled === "string"
      ? { invalidate: true, notice: handled }
      : { ...handled, invalidate: handled?.invalidate ?? true };
  }

  private lookup<T>(map: Map<string, T>, id: string): T {
    const found = map.get(id);
    if (!found) throw new Error(`unknown surface ${id}`);
    return found;
  }

  private contributions(): Contributions {
    return {
      panels: [...this.panels.values()].map((panel) => {
        const size = normalizePaneSize(`panel ${panel.id}`, panel.defaultSize);
        return {
          id: panel.id, title: panel.title, placement: panel.placement ?? "sidebar",
          ...(panel.replaces ? { replaces: panel.replaces } : {}),
          ...(panel.defaultSlot ? { default_slot: panel.defaultSlot } : {}),
          ...(size ? { default_size: size } : {}),
          ...(panel.refreshMs ? { refresh_ms: panel.refreshMs } : {}),
          ...(panel.onKey ? { keys: true as const } : {}),
        };
      }),
      panes: [...this.panes.values()].map((pane) => {
        const size = normalizePaneSize(`pane ${pane.id}`, pane.defaultSize);
        return {
          id: pane.id,
          title: pane.title,
          ...(pane.defaultSlot ? { default_slot: pane.defaultSlot } : {}),
          ...(size ? { default_size: size } : {}),
          ...(pane.pinnable !== undefined ? { pinnable: pane.pinnable } : {}),
          ...(pane.closable !== undefined ? { closable: pane.closable } : {}),
          ...(pane.refreshMs ? { refresh_ms: pane.refreshMs } : {}),
          ...(pane.onKey ? { keys: true as const } : {}),
        };
      }),
      commands: [...this.commands.values()].map((command) => ({
        name: command.name,
        description: command.description,
        ...(command.argumentHint ? { argument_hint: command.argumentHint } : {}),
      })),
      status_items: [...this.statuses.values()].map((item) => ({ id: item.id, label: item.label, priority: item.priority ?? 7 })),
      renderers: [...this.renderers.values()].map((renderer) => ({ id: renderer.id, target: renderer.target, mode: renderer.mode ?? "decorate", priority: renderer.priority ?? 0, ...("tools" in renderer ? { tools: renderer.tools } : {}) })),
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
    /** The host (the TUI) answers in these shapes; it also enforces permissions and confinement authoritatively. */
    const fsCall = <T>(method: string, params: Record<string, unknown>): Promise<T> => {
      if (!this.permissions.includes("fs.read")) return Promise.reject(new Error("missing permission fs.read"));
      if (!this.hostCall) return Promise.reject(new Error("filesystem host unavailable"));
      return this.hostCall(method, params) as Promise<T>;
    };
    const bundleApiRequest = <T extends JsonValue = JsonValue>(options: BundleApiRequestOptions): Promise<T> => {
      if (!this.permissions.includes("tui.bundle_api")) return Promise.reject(new Error("missing permission tui.bundle_api"));
      if (!BUNDLE_API_METHODS.includes(options.method)) return Promise.reject(new Error(`unsupported bundle.api method ${String(options.method)}`));
      const confined = checkBundleConfinement(this.extensionId, options.bundleId, "bundle_api", this.authorizedBundles);
      if (!confined.ok) return Promise.reject(new Error(confined.error));
      const checkedPath = validateBundleApiPath(options.path);
      if (!checkedPath.ok) return Promise.reject(new Error(checkedPath.error));
      if (options.sessionId !== undefined && !isValidSessionId(options.sessionId)) return Promise.reject(new Error("bundle.api sessionId is invalid"));
      if (!this.hostCall) return Promise.reject(new Error("bundle API host unavailable"));
      return this.hostCall("bundle/api", {
        method: options.method,
        path: checkedPath.path,
        bundleId: confined.bundleId,
        ...(options.body !== undefined ? { body: options.body } : {}),
        ...(options.query !== undefined ? { query: options.query } : {}),
        ...(options.sessionId !== undefined ? { sessionId: options.sessionId } : {}),
      }) as Promise<T>;
    };
    const extensionStateGet = <T extends JsonValue = JsonValue>(bundleId?: string): T | undefined => {
      if (!this.permissions.includes("tui.extension_state.read")) throw new Error("missing permission tui.extension_state.read");
      const confined = checkBundleConfinement(this.extensionId, bundleId, "extension_state", this.authorizedBundles);
      if (!confined.ok) throw new Error(confined.error);
      return this.context.extension_state?.[confined.bundleId] as T | undefined;
    };
    const extensionStateRead = <T extends JsonValue = JsonValue>(options?: { readonly sessionId?: string; readonly bundleId?: string }): Promise<T | undefined> => {
      if (!this.permissions.includes("tui.extension_state.read")) return Promise.reject(new Error("missing permission tui.extension_state.read"));
      const confined = checkBundleConfinement(this.extensionId, options?.bundleId, "extension_state", this.authorizedBundles);
      if (!confined.ok) return Promise.reject(new Error(confined.error));
      if (options?.sessionId !== undefined && !isValidSessionId(options.sessionId)) return Promise.reject(new Error("extension_state sessionId is invalid"));
      if (!this.hostCall) return Promise.resolve(this.context.extension_state?.[confined.bundleId] as T | undefined);
      return (this.hostCall("extension_state/read", {
        bundleId: confined.bundleId,
        ...(options?.sessionId !== undefined ? { sessionId: options.sessionId } : {}),
      }) as Promise<{ readonly state?: JsonValue } | null>).then((reply) => {
        if (!reply || reply.state === undefined || reply.state === null) return undefined;
        return reply.state as T;
      });
    };
    return {
      bundleId: this.extensionId,
      registerPanel: (panel) => {
        if (panel.replaces !== undefined && !REPLACEABLE_SURFACES.includes(panel.replaces)) throw new Error(`panel ${panel.id}: cannot replace ${panel.replaces}`);
        if (panel.defaultSlot !== undefined && !PANE_SLOTS.includes(panel.defaultSlot)) throw new Error(`panel ${panel.id}: invalid defaultSlot ${String(panel.defaultSlot)}`);
        normalizePaneSize(`panel ${panel.id}`, panel.defaultSize);
        if (panel.refreshMs !== undefined && (!Number.isSafeInteger(panel.refreshMs) || panel.refreshMs < 1000)) throw new Error(`panel ${panel.id}: refreshMs must be an integer >= 1000`);
        add(this.panels, "panel", panel);
      },
      registerPane: (pane) => {
        if (pane.defaultSlot !== undefined && !PANE_SLOTS.includes(pane.defaultSlot)) throw new Error(`pane ${pane.id}: invalid defaultSlot ${String(pane.defaultSlot)}`);
        normalizePaneSize(`pane ${pane.id}`, pane.defaultSize);
        if (pane.refreshMs !== undefined && (!Number.isSafeInteger(pane.refreshMs) || pane.refreshMs < 1000)) throw new Error(`pane ${pane.id}: refreshMs must be an integer >= 1000`);
        add(this.panes, "pane", pane);
      },
      registerCommand: (command) => {
        if (!this.registering) throw new Error("command must be registered while activate runs");
        if (!commandNamePattern.test(command.name)) throw new Error(`invalid command name ${JSON.stringify(command.name)} (lowercase letters, digits, . _ : -)`);
        if (command.name.startsWith("skill:")) throw new Error(`command ${command.name}: skill: prefix is reserved`);
        if (this.commands.has(command.name)) throw new Error(`duplicate command ${command.name}`);
        if (typeof command.description !== "string" || !command.description.trim() || command.description.length > 160) {
          throw new Error(`command ${command.name}: description must be 1-160 characters`);
        }
        if (command.argumentHint !== undefined && (typeof command.argumentHint !== "string" || command.argumentHint.length > 80)) {
          throw new Error(`command ${command.name}: argumentHint must be at most 80 characters`);
        }
        this.commands.set(command.name, command);
      },
      registerStatusItem: (item) => {
        if (item.priority !== undefined && (item.priority < 1 || item.priority > 9)) throw new Error(`status item ${item.id}: priority must be 1..9`);
        add(this.statuses, "status item", item);
      },
      registerToolRenderer: (renderer) => {
        add(this.renderers, "renderer", { ...renderer, target: "tool_call", mode: "replace" });
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
      bundleApi: {
        request: bundleApiRequest,
        get: (path, options) => bundleApiRequest({ method: "GET", path, ...options }),
        post: (path, body, options) => bundleApiRequest({ method: "POST", path, ...(body !== undefined ? { body } : {}), ...options }),
        put: (path, body, options) => bundleApiRequest({ method: "PUT", path, ...(body !== undefined ? { body } : {}), ...options }),
        patch: (path, body, options) => bundleApiRequest({ method: "PATCH", path, ...(body !== undefined ? { body } : {}), ...options }),
        delete: (path, options) => bundleApiRequest({ method: "DELETE", path, ...options }),
      },
      extensionState: {
        get: extensionStateGet,
        read: extensionStateRead,
      },
    };
  }
}
