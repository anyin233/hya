/**
 * Shared extension wire declarations. Edit packages/hya-tui-sdk/src/wire.ts
 * and run `bun packages/hya-tui-sdk/scripts/copy-wire.ts` from the repo root.
 */

/** Hard upper bound for one newline-delimited RPC frame (UTF-8 bytes). */
export const MAX_FRAME_BYTES = 2 * 1024 * 1024;

/** Version of this authoring SDK, independent of backend/frontend releases. */
export const TUI_EXTENSION_SDK_VERSION = "1.0.0" as const;
/** Incompatible wire changes advance this integer. */
export const TUI_EXTENSION_API_VERSION = 1 as const;
export const JSONRPC_VERSION = "2.0" as const;
/**
 * JSON-RPC error code: the extension's VM stopped serving (a handler ran past
 * its deadline, or a trusted extension's thread died). Every later request
 * gets it too, until the extension is loaded again.
 */
export const VM_STOPPED_ERROR_CODE = -32001;

/** SDK contract major the host speaks. */
export const TUI_EXTENSION_SDK_MAJOR = 1;

export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };
/** String or safe integer; null is reserved for uncorrelated error responses. */
export type RpcId = string | number;

export const TUI_EXTENSION_PERMISSIONS = [
  "tui.panel", "tui.render", "tui.status_item", "tui.session.read",
  "tui.transcript.read", "tui.workspace.read", "workspace.git.read", "tui.action",
  "tui.sessions.read", "tui.projects.read", "tui.todos.read", "tui.status.read",
  "tui.keys", "tui.session.control", "tui.project.control", "fs.read",
] as const;
export type TuiExtensionPermission = (typeof TUI_EXTENSION_PERMISSIONS)[number];

/** Built-in panes a bundle panel may replace (`replaces`). The conversation pane is never replaceable. */
export const REPLACEABLE_PANES = ["projects", "sessions", "todos", "context", "jobs", "status", "models", "workflows", "interactions", "api"] as const;
export type ReplaceablePane = (typeof REPLACEABLE_PANES)[number];
/** Replaceable panes plus the conversation summary and full-screen Projects view. */
export const REPLACEABLE_SURFACES = [...REPLACEABLE_PANES, "context_line", "project_view"] as const;
export type ReplaceableSurface = (typeof REPLACEABLE_SURFACES)[number];

/**
 * Host state an extension may read. Each section is present only when its
 * permission was declared: `session` (`tui.session.read`), `transcript`
 * (`tui.transcript.read`), `workspace` (`tui.workspace.read`), `git`
 * (`workspace.git.read`), `sessions` (`tui.sessions.read`), `projects`
 * (`tui.projects.read`), `todos` (`tui.todos.read`), and `status`
 * (`tui.status.read`). `terminal` is always present.
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
}

/** Where a surface is shown. `sidebar` panels appear automatically; `pane` panels only through `/layout`. */
export type PanelPlacement = "sidebar" | "pane";

/** Everything one extension contributes, reported by `tui/activate`. */
export interface Contributions {
  readonly panels: readonly { readonly id: string; readonly title: string; readonly placement: PanelPlacement; readonly replaces?: ReplaceableSurface; readonly refresh_ms?: number; readonly keys?: true }[];
  readonly status_items: readonly { readonly id: string; readonly label: string; readonly priority: number }[];
  readonly renderers: readonly { readonly id: string; readonly target: "tool_call" | "composer"; readonly mode: "replace" | "decorate"; readonly priority: number }[];
  readonly formatters: readonly { readonly id: string; readonly priority: number }[];
  readonly interceptors: readonly { readonly id: string; readonly target: "submit"; readonly priority: number }[];
}

/** A renderable contribution addressed by `tui/render` and `tui/action`. */
export interface SurfaceRef {
  readonly kind: "panel" | "status" | "renderer";
  readonly id: string;
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
    params: { readonly api_version: typeof TUI_EXTENSION_API_VERSION; readonly sdk_version: string; readonly extension_id: string; readonly permissions: readonly TuiExtensionPermission[] };
    result: { readonly api_version: typeof TUI_EXTENSION_API_VERSION; readonly sdk_version: string };
  };
  "tui/activate": {
    params: { readonly context: ExtensionContext };
    result: { readonly contributions: Contributions };
  };
  "tui/render": {
    params: { readonly surface: SurfaceRef; readonly width: number; readonly height: number; readonly now: number; readonly input?: JsonValue };
    /** `null` root: hide (panel, status) or fall back to the built-in rendering (renderer). */
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

export type RenderNode =
  | { readonly kind: "text"; readonly text: string; readonly style?: TextStyle; readonly action?: NodeAction }
  | { readonly kind: "row" | "column"; readonly children: readonly RenderNode[]; readonly gap?: number }
  | { readonly kind: "box"; readonly children: readonly RenderNode[]; readonly title?: string; readonly border?: boolean; readonly padding?: number }
  | { readonly kind: "table"; readonly columns: readonly string[]; readonly rows: readonly (readonly string[])[] }
  | { readonly kind: "progress"; readonly value: number; readonly total: number; readonly label?: string }
  /** Decorating renderers only: where the built-in (or lower-priority) rendering goes. Exactly one per tree. */
  | { readonly kind: "slot" };

/** Host limits for one tree; larger trees are rejected. */
export const RENDER_LIMITS = { maxDepth: 32, maxNodes: 2_000, maxStringBytes: 32 * 1024 } as const;

/** Maximum number of contributions of each kind in one activation. */
export const MAX_CONTRIBUTIONS = 32;
