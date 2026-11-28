/**
 * Shared extension wire declarations. Edit packages/hya-tui-sdk/src/wire.ts
 * and run `bun packages/hya-tui-sdk/scripts/copy-wire.ts` from the repo root.
 *
 * Pure ECMAScript only (no Bun/Node/DOM globals): this file is bundled into
 * QuickJS-WASM extension VMs as well as imported by the Bun TUI host.
 */

/** Hard upper bound for one newline-delimited RPC frame (UTF-8 bytes). */
export const MAX_FRAME_BYTES = 2 * 1024 * 1024;

/** Version of this authoring SDK, independent of backend/frontend releases. */
export const TUI_EXTENSION_SDK_VERSION = "2.0.0" as const;
/** Incompatible wire changes advance this integer. */
export const TUI_EXTENSION_API_VERSION = 2 as const;
export const JSONRPC_VERSION = "2.0" as const;
/**
 * JSON-RPC error code: the extension's VM stopped serving (a handler ran past
 * its deadline, or a trusted extension's thread died). Every later request
 * gets it too, until the extension is loaded again.
 */
export const VM_STOPPED_ERROR_CODE = -32001;

/** SDK contract major the host speaks. */
export const TUI_EXTENSION_SDK_MAJOR = 2;

export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };
/** String or safe integer; null is reserved for uncorrelated error responses. */
export type RpcId = string | number;

export const TUI_EXTENSION_PERMISSIONS = [
  "tui.panel", "tui.render", "tui.status_item", "tui.session.read",
  "tui.transcript.read", "tui.transcript.embed", "tui.workspace.read", "workspace.git.read",
  "tui.action", "tui.sessions.read", "tui.projects.read", "tui.todos.read", "tui.status.read",
  "tui.keys", "tui.session.control", "tui.project.control", "fs.read",
  "tui.bundle_api", "tui.extension_state.read",
] as const;
export type TuiExtensionPermission = (typeof TUI_EXTENSION_PERMISSIONS)[number];

/** Built-in panes a bundle panel may replace (`replaces`). The conversation pane is never replaceable. */
export const REPLACEABLE_PANES = ["projects", "sessions", "todos", "context", "jobs", "status", "models", "workflows", "interactions", "api"] as const;
export type ReplaceablePane = (typeof REPLACEABLE_PANES)[number];
/** Replaceable panes plus the conversation summary and full-screen Projects view. */
export const REPLACEABLE_SURFACES = [...REPLACEABLE_PANES, "context_line", "project_view"] as const;
export type ReplaceableSurface = (typeof REPLACEABLE_SURFACES)[number];

/** Layout slots where a dynamic named pane may open by default. */
export const PANE_SLOTS = ["left", "right", "top", "bottom", "main", "overlay"] as const;
export type PaneSlot = (typeof PANE_SLOTS)[number];

/** HTTP methods accepted by `bundle.api` and `api.bundleApi`. */
export const BUNDLE_API_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE"] as const;
export type BundleApiMethod = (typeof BUNDLE_API_METHODS)[number];

/** Capabilities subject to strict bundle-ID confinement. */
export type BundleScopedCapability = "bundle_api" | "extension_state";

/** Preferred dimensions for a dynamic named pane. */
export interface PaneSize {
  /** Default width in columns (`1..1000`) or percentage string (e.g. `"35%"`). */
  readonly width?: number | string;
  /** Default height in rows (`1..1000`) or percentage string (e.g. `"40%"`). */
  readonly height?: number | string;
  /** Minimum width in columns (`1..1000`). */
  readonly min_width?: number;
  /** Minimum height in rows (`1..1000`). */
  readonly min_height?: number;
}

/**
 * Host state an extension may read. Each section is present only when its
 * permission was declared: `session` (`tui.session.read`), `transcript`
 * (`tui.transcript.read`), `workspace` (`tui.workspace.read`), `git`
 * (`workspace.git.read`), `sessions` (`tui.sessions.read`), `projects`
 * (`tui.projects.read`), `todos` (`tui.todos.read`), `status`
 * (`tui.status.read`), and `extension_state` (`tui.extension_state.read`,
 * confined to the caller's own `bundleId` unless explicitly authorized).
 * `terminal` is always present.
 */
export interface ExtensionContext {
  readonly terminal: { readonly columns: number; readonly rows: number };
  readonly session?: {
    readonly id?: string; readonly title?: string; readonly agent?: string; readonly model?: string;
    readonly mode?: string; readonly workdir?: string; readonly busy: boolean;
  };
  /** The open session's latest messages (at most 50), oldest first. */
  readonly transcript?: readonly { readonly role: string; readonly text: string }[];
  readonly workspace?: { readonly directory: string; readonly project?: { readonly id: string; readonly name?: string } };
  readonly git?: { readonly branch?: string; readonly head?: string; readonly dirty: number; readonly ahead: number; readonly behind: number };
  readonly sessions?: {
    readonly ready: boolean;
    readonly selected?: string;
    /** Sessions in the sidebar's project scope, in host order. */
    readonly items: readonly {
      readonly id: string; readonly title?: string; readonly agent: string;
      readonly temporary: boolean; readonly archived: boolean; readonly busy: boolean;
      readonly waiting: boolean; readonly parent?: string; readonly created?: number;
      readonly members?: readonly { readonly child: string; readonly handle?: string; readonly description?: string; readonly agent?: string }[];
    }[];
  };
  readonly projects?: {
    readonly ready: boolean;
    readonly active?: string;
    /** Why the last read of the Project list failed (`<code>: <message>`), until one succeeds. */
    readonly error?: string;
    readonly items: readonly { readonly id: string; readonly name: string; readonly roots: readonly string[]; readonly busy: boolean; readonly sessionCount?: number }[];
  };
  /** The open session's todo list, in order. */
  readonly todos?: readonly { readonly status: "pending" | "in_progress" | "blocked" | "completed"; readonly content: string }[];
  /**
   * The facts behind the Context box, already resolved by the host (labels it
   * derives from its own state: mode, fork source, model, WebUI). The
   * extension chooses fields, order, colors, and truncation.
   */
  readonly status?: {
    /** The session list has loaded (before that the Session field reads `connecting…`). */
    readonly ready: boolean;
    /** Present while Vim mode is on. */
    readonly vim?: { readonly normal: boolean; readonly pending?: string };
    readonly mode: { readonly text: string; readonly tone: "strong" | "accent" | "error" };
    readonly session?: {
      readonly id: string; readonly title?: string; readonly agent: string;
      /** `forked from <title or id>`. */
      readonly forked?: string;
      /** `provider/model:effort` and `model:effort`; empty when the server chose no model. */
      readonly model: string; readonly modelShort: string;
      readonly messages: number; readonly workdir?: string;
      /** Occupancy of the model's context window by the latest round. */
      readonly context?: { readonly percent: number; readonly tokens: number; readonly limit: number };
      /** Everything billed for the session. */
      readonly tokens?: number;
    };
    readonly branch?: string;
    readonly todos?: { readonly done: number; readonly total: number };
    /** Status items contributed by every running extension, rendered by the host. */
    readonly items: readonly { readonly label: string; readonly text: string; readonly priority: number }[];
    /** Server host (`127.0.0.1:8080`, or its label). */
    readonly server: string;
    /** The WebUI: its host when it runs, and the status line label (`WebUI unavailable`, …). */
    readonly web?: { readonly host?: string; readonly label: string };
    readonly versions: { readonly tui: string; readonly backend?: string };
    readonly connection: "connected" | "disconnected" | "stopped";
  };
  /**
   * Folded per-bundle session state (`Projection::extensions`), gated by
   * `tui.extension_state.read`. Strictly confined to the caller's `bundleId`
   * unless explicitly authorized by the host.
   */
  readonly extension_state?: Readonly<Record<string, JsonValue>>;
}

/** Where a surface is shown. `sidebar` panels appear automatically; `pane` panels only through `/layout`. */
export type PanelPlacement = "sidebar" | "pane";

/** Wire descriptor for a dynamic named pane contributed via `api.registerPane`. */
export interface PaneDescriptor {
  readonly id: string;
  readonly title: string;
  readonly default_slot?: PaneSlot;
  readonly default_size?: PaneSize;
  readonly pinnable?: boolean;
  readonly closable?: boolean;
  readonly refresh_ms?: number;
  readonly keys?: true;
}

/** Wire descriptor for a slash command contributed via `api.registerCommand`. */
export interface SlashCommandDescriptor {
  /** Command name without the leading `/` (cannot start with `skill:`). */
  readonly name: string;
  readonly description: string;
  /** Optional argument hint shown in completion UI (e.g. `<id>` or `[filter]`). */
  readonly argument_hint?: string;
}

/** Everything one extension contributes, reported by `tui/activate`. */
export interface Contributions {
  readonly panels: readonly {
    readonly id: string;
    readonly title: string;
    readonly placement: PanelPlacement;
    readonly replaces?: ReplaceableSurface;
    readonly default_slot?: PaneSlot;
    readonly default_size?: PaneSize;
    readonly refresh_ms?: number;
    readonly keys?: true;
  }[];
  /** Dynamic named panes opened/pinned/closed/focused via `pane.*` commands or slash commands. */
  readonly panes: readonly PaneDescriptor[];
  /** Slash commands registered by this extension (invoked via `tui/command`). */
  readonly commands: readonly SlashCommandDescriptor[];
  readonly status_items: readonly { readonly id: string; readonly label: string; readonly priority: number }[];
  readonly renderers: readonly { readonly id: string; readonly target: "tool_call" | "composer"; readonly mode: "replace" | "decorate"; readonly priority: number }[];
  readonly formatters: readonly { readonly id: string; readonly priority: number }[];
  readonly interceptors: readonly { readonly id: string; readonly target: "submit"; readonly priority: number }[];
}

/** A renderable or command contribution addressed by `tui/render`, `tui/action`, `tui/key`, and `tui/command_result`. */
export interface SurfaceRef {
  readonly kind: "panel" | "pane" | "status" | "renderer" | "command";
  readonly id: string;
  /** Optional instance identifier for multi-instance dynamic panes (e.g. a subsession id). */
  readonly instance?: string;
}

/** `tui/render` input of a `tool_call` renderer. `input`/`output`/`error` need `tui.transcript.read`. */
export interface ToolCallInput {
  readonly id: string;
  readonly tool: string;
  readonly status: "pending" | "running" | "completed" | "error";
  readonly summary: string;
  readonly input?: JsonValue;
  readonly output?: JsonValue;
  readonly error?: string;
}

/** `tui/format` input: the tool output value and the text the host (or a lower-priority formatter) produced. */
export interface FormatInput {
  readonly value: JsonValue;
  readonly text: string;
}

export type InterceptDecision =
  | { readonly decision: "continue" }
  | { readonly decision: "replace"; readonly text: string }
  | { readonly decision: "block"; readonly message: string };

export interface ExtensionAction {
  readonly name: string;
  readonly data: JsonValue;
  readonly button: "left" | "right";
}

export interface KeyEvent {
  readonly name: string; readonly sequence: string; readonly ctrl: boolean; readonly shift: boolean; readonly meta: boolean;
}

/** Bundle API command payload (strictly confined to the caller's `bundleId` unless explicitly authorized). */
export interface BundleApiCommand {
  readonly command: "bundle.api";
  readonly method: BundleApiMethod;
  /** Path below `/v2/bundles/<bundleId>/api` (must start with `/`, no `..`). */
  readonly path: string;
  readonly body?: JsonValue;
  readonly query?: Readonly<Record<string, string>>;
  /** Optional session ID to invoke `/v2/sessions/<sessionId>/bundles/<bundleId><path>` instead of global scope. */
  readonly sessionId?: string;
  /** Optional explicit target bundle ID; rejected if different from caller's `bundleId` unless explicitly authorized. */
  readonly bundleId?: string;
  readonly token?: string;
}

/** Dynamic named pane lifecycle commands. */
export type PaneHostCommand =
  | {
      readonly command: "pane.open";
      readonly pane: string;
      readonly instance?: string;
      readonly title?: string;
      readonly slot?: PaneSlot;
      readonly size?: PaneSize;
      readonly params?: JsonValue;
      readonly focus?: boolean;
      readonly token?: string;
    }
  | {
      readonly command: "pane.pin";
      readonly pane: string;
      readonly instance?: string;
      readonly pinned?: boolean;
      readonly token?: string;
    }
  | {
      readonly command: "pane.close";
      readonly pane: string;
      readonly instance?: string;
      readonly token?: string;
    }
  | {
      readonly command: "pane.focus";
      readonly pane: string;
      readonly instance?: string;
      readonly token?: string;
    };

export type HostCommand =
  | { readonly command: "session.open"; readonly id: string; readonly token?: string }
  | { readonly command: "session.menu"; readonly id: string }
  | { readonly command: "session.new_temporary"; readonly token?: string }
  | { readonly command: "project.switch"; readonly id: string; readonly token?: string }
  | { readonly command: "project.menu"; readonly id: string }
  | { readonly command: "project.create"; readonly name: string; readonly roots: readonly string[]; readonly token?: string }
  | { readonly command: "project.rename"; readonly id: string; readonly name: string; readonly token?: string }
  | { readonly command: "project.set_roots"; readonly id: string; readonly roots: readonly string[]; readonly token?: string }
  | { readonly command: "project.delete"; readonly id: string; readonly token?: string }
  | { readonly command: "fs.complete"; readonly input: string; readonly token: string }
  | BundleApiCommand
  | PaneHostCommand
  | { readonly command: "ui.release" }
  | { readonly command: "ui.close" }
  /** Show this panel as the overlay again; only the `project_view` replacement may (it closes before switching, and reopens on failure). */
  | { readonly command: "ui.open" };

export interface CommandResult {
  readonly token: string; readonly ok: boolean; readonly error?: string; readonly value?: JsonValue;
}

export interface FsEvent { readonly path: string; readonly kind: "change" | "rename" | "closed" }

/** Host-to-extension calls. Every payload is JSON data, never executable code. */
export interface TuiMethodMap {
  "tui/initialize": {
    params: {
      readonly api_version: typeof TUI_EXTENSION_API_VERSION;
      readonly sdk_version: string;
      readonly extension_id: string;
      readonly permissions: readonly TuiExtensionPermission[];
      /** Optional list of additional bundle IDs explicitly authorized for cross-bundle `bundle_api` / `extension_state`. */
      readonly authorized_bundles?: readonly string[];
    };
    result: { readonly api_version: typeof TUI_EXTENSION_API_VERSION; readonly sdk_version: string };
  };
  "tui/activate": {
    params: { readonly context: ExtensionContext };
    result: { readonly contributions: Contributions };
  };
  "tui/render": {
    params: { readonly surface: SurfaceRef; readonly width: number; readonly height: number; readonly now: number; readonly input?: JsonValue };
    /** `null` root: hide (panel, pane, status) or fall back to the built-in rendering (renderer). */
    result: { readonly root: RenderNode | null };
  };
  "tui/format": {
    params: { readonly formatter: string; readonly input: FormatInput; readonly width: number };
    result: { readonly text: string | null };
  };
  "tui/intercept": {
    params: { readonly interceptor: string; readonly input: { readonly text: string } };
    result: InterceptDecision;
  };
  "tui/command": {
    params: { readonly name: string; readonly args: string; readonly raw: string };
    result: TuiMethodMap["tui/action"]["result"];
  };
  "tui/context": {
    params: { readonly context: ExtensionContext };
    /** `invalidate: false` keeps the extension's rendered surfaces. */
    result: { readonly invalidate: boolean };
  };
  "tui/action": {
    params: { readonly surface: SurfaceRef; readonly action: ExtensionAction };
    result: { readonly invalidate: boolean; readonly notice?: string; readonly commands?: readonly HostCommand[] };
  };
  "tui/key": {
    params: { readonly surface: SurfaceRef; readonly key: KeyEvent };
    result: TuiMethodMap["tui/action"]["result"];
  };
  "tui/command_result": {
    params: { readonly surface: SurfaceRef; readonly result: CommandResult };
    result: TuiMethodMap["tui/action"]["result"];
  };
  "tui/shutdown": {
    params: Record<string, never>;
    result: null;
  };
  "tui/fs_event": {
    params: { readonly watch: number; readonly events: readonly FsEvent[] };
    result: TuiMethodMap["tui/action"]["result"];
  };
}

export type TuiMethod = keyof TuiMethodMap;
export type TuiRequest<M extends TuiMethod = TuiMethod> = {
  [K in M]: {
    readonly jsonrpc: typeof JSONRPC_VERSION;
    readonly id: RpcId;
    readonly method: K;
    readonly params: TuiMethodMap[K]["params"];
  }
}[M];

export interface RpcError {
  readonly code: number;
  readonly message: string;
  readonly data?: JsonValue;
}

export type TuiResponse<M extends TuiMethod = TuiMethod> =
  | { readonly jsonrpc: typeof JSONRPC_VERSION; readonly id: RpcId; readonly result: TuiMethodMap[M]["result"]; readonly error?: never }
  | { readonly jsonrpc: typeof JSONRPC_VERSION; readonly id: RpcId | null; readonly error: RpcError; readonly result?: never };

export type TuiFrame = TuiRequest | TuiResponse;

/** Parsed envelope only; payload validation/dispatch belongs to the consumer. */
export type RpcEnvelope =
  | { readonly jsonrpc: "2.0"; readonly id: RpcId; readonly method: string; readonly params: JsonValue }
  | { readonly jsonrpc: "2.0"; readonly id: RpcId; readonly result: JsonValue }
  | { readonly jsonrpc: "2.0"; readonly id: RpcId | null; readonly error: RpcError };

/**
 * Declarative content only: no callbacks, terminal escapes, or OpenTUI objects.
 * The host validates every tree (kinds, fields, depth, size) and strips terminal
 * escapes; an invalid tree is shown as an error instead of the surface.
 */
export interface TextStyle {
  /** `#rrggbb` or a theme token: `fg`, `muted`, `focus`, `heading`, `link`, `activity`, `success`, `warning`, `error`, `border`. `accent` is a legacy alias for `focus`. */
  readonly color?: string;
  readonly background?: string;
  readonly bold?: boolean;
  readonly italic?: boolean;
  readonly underline?: boolean;
}

/** A click on the node sends `tui/action` with this name and data (needs `tui.action`). */
export interface NodeAction {
  readonly name: string;
  readonly data?: JsonValue;
}

/**
 * Host-virtualized streaming transcript node (requires `tui.transcript.embed`).
 * Rendered directly by the host from the session's live projection/overlay
 * without cloning transcript trees over IPC.
 */
export interface TranscriptNodeProps {
  readonly sessionId: string;
  /** Keep scrolled to the latest message as the session streams (default `true`). */
  readonly follow?: boolean;
  /** Explicit scroll offset in lines from the top (used when `follow` is `false`). */
  readonly scrollOffset?: number;
  /** Optional cap on recent messages rendered (`1..1000`). */
  readonly maxMessages?: number;
  /** Compact rendering mode (e.g. condensed tool cards) for narrow side panes. */
  readonly compact?: boolean;
}

export type TranscriptRenderNode =
  | ({ readonly type: "transcript"; readonly kind?: "transcript" } & TranscriptNodeProps)
  | ({ readonly kind: "transcript"; readonly type?: "transcript" } & TranscriptNodeProps);

export type RenderNode =
  | { readonly kind: "text"; readonly text: string; readonly style?: TextStyle; readonly action?: NodeAction }
  | { readonly kind: "row" | "column"; readonly children: readonly RenderNode[]; readonly gap?: number }
  | { readonly kind: "box"; readonly children: readonly RenderNode[]; readonly title?: string; readonly border?: boolean; readonly padding?: number }
  | { readonly kind: "table"; readonly columns: readonly string[]; readonly rows: readonly (readonly string[])[] }
  | { readonly kind: "progress"; readonly value: number; readonly total: number; readonly label?: string }
  /** Decorating renderers only: where the built-in (or lower-priority) rendering goes. Exactly one per tree. */
  | { readonly kind: "slot" }
  /** Host-virtualized streaming transcript view (requires `tui.transcript.embed`). */
  | TranscriptRenderNode;

/** Host limits for one tree; larger trees are rejected. */
export const RENDER_LIMITS = { maxDepth: 32, maxNodes: 2_000, maxStringBytes: 32 * 1024 } as const;

/** Maximum number of contributions of each kind in one activation. */
export const MAX_CONTRIBUTIONS = 32;

// Shared pure-ES validation & confinement helpers -------------------------

const bundleIdPattern = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$/;
const sessionIdPattern = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const controlChars = /[\u0000-\u001F\u007F-\u009F]/;

/** Type guard for host-virtualized transcript render nodes (`{ type: "transcript", sessionId }` or `{ kind: "transcript", sessionId }`). */
export function isTranscriptNode(node: RenderNode): node is TranscriptRenderNode {
  return ("type" in node && node.type === "transcript") || node.kind === "transcript";
}

/**
 * Enforce strict bundle-ID confinement for `bundle.api` and `extension_state`.
 * Defaults to the caller's own `bundleId` when `targetBundleId` is omitted, and
 * rejects any cross-bundle target unless explicitly authorized.
 */
export function checkBundleConfinement(
  callerBundleId: string,
  targetBundleId: string | undefined,
  capability: BundleScopedCapability,
  authorized?: readonly string[] | ((target: string) => boolean),
): { readonly ok: true; readonly bundleId: string } | { readonly ok: false; readonly error: string } {
  if (!callerBundleId || !bundleIdPattern.test(callerBundleId)) {
    return { ok: false, error: `${capability}: caller bundleId is unavailable or invalid` };
  }
  const target = targetBundleId === undefined || targetBundleId === "" ? callerBundleId : targetBundleId;
  if (!bundleIdPattern.test(target)) {
    return { ok: false, error: `${capability}: invalid target bundleId ${JSON.stringify(targetBundleId)}` };
  }
  if (target === callerBundleId) return { ok: true, bundleId: target };
  const allowed = typeof authorized === "function"
    ? authorized(target)
    : Array.isArray(authorized) && authorized.includes(target);
  if (!allowed) {
    return { ok: false, error: `${capability} confined to bundle ${callerBundleId}; cannot access ${target}` };
  }
  return { ok: true, bundleId: target };
}

/** Validate a relative API path below `/v2/bundles/<bundleId>/api` (prevents traversal and smuggling). */
export function validateBundleApiPath(
  path: unknown,
): { readonly ok: true; readonly path: string } | { readonly ok: false; readonly error: string } {
  if (typeof path !== "string" || !path || path.length > 512) {
    return { ok: false, error: "bundle.api path must be a non-empty string (<= 512 chars)" };
  }
  if (!path.startsWith("/")) {
    return { ok: false, error: "bundle.api path must start with '/'" };
  }
  if (controlChars.test(path) || path.includes("\\") || path.includes("?") || path.includes("#") || /\s/.test(path)) {
    return { ok: false, error: "bundle.api path contains forbidden characters" };
  }
  const segments = path.slice(1).split("/");
  for (const segment of segments) {
    if (segment === "" && path !== "/") {
      return { ok: false, error: "bundle.api path must not contain empty segments or trailing slash" };
    }
    if (segment === "." || segment === "..") {
      return { ok: false, error: "bundle.api path must not contain traversal segments" };
    }
    let decoded: string;
    try {
      decoded = decodeURIComponent(segment);
    } catch {
      return { ok: false, error: "bundle.api path contains malformed percent-encoding" };
    }
    if (decoded === "." || decoded === ".." || decoded.includes("/") || decoded.includes("\\") || controlChars.test(decoded)) {
      return { ok: false, error: "bundle.api path contains encoded traversal or control characters" };
    }
  }
  return { ok: true, path };
}

/** Validate a session ID used in `bundle.api`, `extension_state`, or `transcript` nodes. */
export function isValidSessionId(value: unknown): value is string {
  return typeof value === "string" && sessionIdPattern.test(value);
}

/**
 * Construct the confined HTTP path for a `bundle.api` invocation:
 * - Global: `/v2/bundles/<encodeURIComponent(bundleId)>/api<path>`
 * - Session-scoped: `/v2/sessions/<encodeURIComponent(sessionId)>/bundles/<encodeURIComponent(bundleId)><path>`
 */
export function buildBundleApiUrlPath(
  bundleId: string,
  path: unknown,
  options?: { readonly sessionId?: string; readonly query?: Readonly<Record<string, string>> },
): { readonly ok: true; readonly urlPath: string; readonly normalizedPath: string } | { readonly ok: false; readonly error: string } {
  if (!bundleId || !bundleIdPattern.test(bundleId)) {
    return { ok: false, error: `invalid bundleId ${JSON.stringify(bundleId)}` };
  }
  const checkedPath = validateBundleApiPath(path);
  if (!checkedPath.ok) return checkedPath;
  if (options?.sessionId !== undefined && !isValidSessionId(options.sessionId)) {
    return { ok: false, error: "bundle.api sessionId is invalid" };
  }
  let suffix = "";
  if (options?.query !== undefined) {
    if (!options.query || typeof options.query !== "object" || Array.isArray(options.query)) {
      return { ok: false, error: "bundle.api query must be a string record" };
    }
    const entries = Object.entries(options.query);
    if (entries.length > 64) return { ok: false, error: "bundle.api query has too many parameters" };
    const pairs: string[] = [];
    for (const [key, value] of entries) {
      if (typeof value !== "string" || !key || controlChars.test(key) || controlChars.test(value)) {
        return { ok: false, error: "bundle.api query keys and values must be safe strings" };
      }
      pairs.push(`${encodeURIComponent(key)}=${encodeURIComponent(value)}`);
    }
    if (pairs.length > 0) suffix = `?${pairs.join("&")}`;
  }
  const encodedBundle = encodeURIComponent(bundleId);
  const base = options?.sessionId
    ? `/v2/sessions/${encodeURIComponent(options.sessionId)}/bundles/${encodedBundle}${checkedPath.path}`
    : `/v2/bundles/${encodedBundle}/api${checkedPath.path}`;
  return { ok: true, urlPath: `${base}${suffix}`, normalizedPath: checkedPath.path };
}

/** Validate a host-virtualized transcript node (`{ type: "transcript", sessionId, ... }`). */
export function validateTranscriptNode(
  raw: Readonly<Record<string, unknown>>,
  options: { readonly allowed: boolean },
): TranscriptRenderNode | string {
  if (!options.allowed) return "transcript node requires permission tui.transcript.embed";
  const allowedKeys = ["type", "kind", "sessionId", "follow", "scrollOffset", "maxMessages", "compact"];
  if (!Object.keys(raw).every((key) => allowedKeys.includes(key))) return "transcript: unknown field";
  if (raw.type !== undefined && raw.type !== "transcript") return "transcript.type must be 'transcript'";
  if (raw.kind !== undefined && raw.kind !== "transcript") return "transcript.kind must be 'transcript'";
  if (!isValidSessionId(raw.sessionId)) return "transcript.sessionId must be a valid session id";
  if (raw.follow !== undefined && typeof raw.follow !== "boolean") return "transcript.follow must be boolean";
  if (raw.scrollOffset !== undefined && (!Number.isSafeInteger(raw.scrollOffset) || (raw.scrollOffset as number) < 0)) {
    return "transcript.scrollOffset must be a non-negative integer";
  }
  if (raw.maxMessages !== undefined && (!Number.isSafeInteger(raw.maxMessages) || (raw.maxMessages as number) < 1 || (raw.maxMessages as number) > 1000)) {
    return "transcript.maxMessages must be an integer in 1..1000";
  }
  if (raw.compact !== undefined && typeof raw.compact !== "boolean") return "transcript.compact must be boolean";
  return {
    type: "transcript",
    sessionId: raw.sessionId,
    ...(raw.follow !== undefined ? { follow: raw.follow as boolean } : {}),
    ...(raw.scrollOffset !== undefined ? { scrollOffset: raw.scrollOffset as number } : {}),
    ...(raw.maxMessages !== undefined ? { maxMessages: raw.maxMessages as number } : {}),
    ...(raw.compact !== undefined ? { compact: raw.compact as boolean } : {}),
  };
}
