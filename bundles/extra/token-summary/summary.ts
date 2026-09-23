// hya-extra/token-summary — a self-contained Bun script that speaks the hya
// plugin protocol v1 (newline-delimited JSON-RPC 2.0 over stdio) directly.
//
// Declared with `extensions.process: { kind: bun, command: [...] }`, so it
// does NOT get the `hya-plugin-bun` adapter injected (that only happens for
// implicit JavaScript Plugins) — this file owns the whole wire surface it
// needs: `initialize`, `view/get` (view `usage`), and `tool/call` (tool
// `token_summary`). Both answer by reading the request-scoped `session.usage`
// host capability (docs/plugin-protocol.md#request-scoped-host-capabilities).
//
// Reading a capability answer means this process must itself act as a JSON-RPC
// *client* on the same stdio connection it is served on: it sends
// `host/capability` requests (with ids from its own counter) and must keep
// reading incoming lines while one is outstanding, because the host may also
// send this process a *new* request (another `view/get` or `tool/call`) before
// that reply arrives. So the stdio loop below never blocks on a single
// in-flight exchange — it classifies every incoming line by shape (a
// `method` field means a new host request/notification; a bare `result` /
// `error` means a reply to one of our own outgoing requests) and dispatches
// each independently. See `CapabilityClient` below.
//
// Pure logic (`buildView`, `renderTable`, `parseViewQuery`, `parseToolInput`,
// `CapabilityClient`) is exported for `bun test` and does not touch stdio;
// the process loop is guarded by `import.meta.main` so importing this file
// for tests never starts it.

import { createInterface } from "node:readline";

const PLUGIN_ID = "token-summary";
const PLUGIN_VERSION = "1.0.0";
const VIEW_ID = "usage";
const TOOL_ID = "token_summary";
const GENERATED_BY = "hya-extra/token-summary";

// --------------------------------------------------------------- wire types

/** `UsageTotals` (see docs/plugin-protocol.md#request-scoped-host-capabilities). */
export interface WireUsageTotals {
  input: number;
  cache_read: number;
  cache_write: number;
  output: number;
  reasoning: number;
  reasoning_unknown_output: number;
  rounds: number;
  legacy_messages: number;
}

/** `output` split into thinking / visible / unknown; `thinking + visible + unknown == output`. */
export interface WireOutputSplit {
  thinking: number;
  visible: number;
  unknown: number;
}

/** `UsageTotals` flattened with its `split`. */
export type WireUsageTotalsReport = WireUsageTotals & { split: WireOutputSplit };

export interface WireUsageReport {
  by_model: Record<string, WireUsageTotalsReport>;
  by_purpose: Record<string, WireUsageTotalsReport>;
  total: WireUsageTotalsReport;
}

export interface WireSessionUsageRow {
  session: string;
  parent?: string;
  agent?: string;
  usage: WireUsageReport;
}

/** The `session.usage` host capability result. */
export interface WireSessionUsageReport {
  session: string;
  scope: "session" | "tree" | "root";
  root: string;
  sessions: WireSessionUsageRow[];
  total: WireUsageReport;
  truncated?: boolean;
}

// -------------------------------------------------------------- view shape

/** One model row (or the grand total, minus `model`) of the `usage` view. */
export interface ModelFields {
  input: number;
  cache_creation: number;
  cache_read: number;
  output: number;
  /** `null` when any contributing call's thinking split is unknown (for example Anthropic). */
  thinking: number | null;
  /** `null` under the same condition as `thinking`. */
  visible_output: number | null;
  /** `output` of calls whose split is unknown; `0` when the split is fully known. */
  unsplit_output: number;
  rounds: number;
  /** `input + cache_read + cache_creation`. */
  prompt_total: number;
}

export type ModelRow = ModelFields & { model: string };

export interface SessionRow {
  session: string;
  parent?: string;
  agent?: string;
  models: ModelRow[];
  total: ModelFields;
}

export interface ViewJson {
  session: string;
  scope: string;
  generated_by: typeof GENERATED_BY;
  models: ModelRow[];
  total: ModelFields;
  sessions: SessionRow[];
  truncated?: boolean;
}

/** `UsageTotalsReport` -> the view's flat numeric fields (`cache_write` renamed `cache_creation`). */
export function totalsToFields(totals: WireUsageTotalsReport): ModelFields {
  const unknown = totals.split.unknown;
  const known = unknown === 0;
  return {
    input: totals.input,
    cache_creation: totals.cache_write,
    cache_read: totals.cache_read,
    output: totals.output,
    thinking: known ? totals.split.thinking : null,
    visible_output: known ? totals.split.visible : null,
    unsplit_output: unknown,
    rounds: totals.rounds,
    prompt_total: totals.input + totals.cache_read + totals.cache_write,
  };
}

/** Sort key: total prompt + output tokens, descending, then model name ascending. */
function sortModels(rows: ModelRow[]): ModelRow[] {
  return [...rows].sort((a, b) => {
    const scoreA = a.prompt_total + a.output;
    const scoreB = b.prompt_total + b.output;
    if (scoreB !== scoreA) return scoreB - scoreA;
    if (a.model < b.model) return -1;
    if (a.model > b.model) return 1;
    return 0;
  });
}

function modelRows(byModel: Record<string, WireUsageTotalsReport>): ModelRow[] {
  const rows = Object.entries(byModel).map(([model, totals]) => ({
    model,
    ...totalsToFields(totals),
  }));
  return sortModels(rows);
}

/** Transform a `session.usage` capability result into the `usage` view/tool JSON. */
export function buildView(report: WireSessionUsageReport): ViewJson {
  const view: ViewJson = {
    session: report.session,
    scope: report.scope,
    generated_by: GENERATED_BY,
    models: modelRows(report.total.by_model),
    total: totalsToFields(report.total.total),
    sessions: report.sessions.map((row) => ({
      session: row.session,
      ...(row.parent === undefined ? {} : { parent: row.parent }),
      ...(row.agent === undefined ? {} : { agent: row.agent }),
      models: modelRows(row.usage.by_model),
      total: totalsToFields(row.usage.total),
    })),
  };
  if (report.truncated) view.truncated = true;
  return view;
}

// -------------------------------------------------------- markdown table

function cell(value: number | null): string {
  return value === null ? "—" : String(value);
}

/**
 * Render a compact Markdown table: one row per model, a totals row, then
 * (when `scope` covers more than one session) one line per session.
 */
export function renderTable(view: ViewJson, scope: string): string {
  const lines: string[] = [];
  lines.push(`Token usage (scope: ${scope})`);
  lines.push("");
  lines.push("| model | input | cache creation | cache read | output | thinking | visible | rounds |");
  lines.push("| --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const row of view.models) {
    lines.push(
      `| ${row.model} | ${row.input} | ${row.cache_creation} | ${row.cache_read} | ${row.output} | ${cell(row.thinking)} | ${cell(row.visible_output)} | ${row.rounds} |`,
    );
  }
  const total = view.total;
  lines.push(
    `| **total** | ${total.input} | ${total.cache_creation} | ${total.cache_read} | ${total.output} | ${cell(total.thinking)} | ${cell(total.visible_output)} | ${total.rounds} |`,
  );
  if (scope !== "session" && view.sessions.length > 0) {
    lines.push("");
    lines.push("Sessions:");
    for (const row of view.sessions) {
      const label = row.agent ? `${row.session} (${row.agent})` : row.session;
      lines.push(
        `- ${label}: input ${row.total.input}, output ${row.total.output}, rounds ${row.total.rounds}`,
      );
    }
  }
  return lines.join("\n");
}

// ---------------------------------------------------------------- input

export type ViewQuery =
  | { ok: true; scope: "session" | "tree" }
  | { ok: false; error: string };

/** Validate `view/get` query params: `scope` (`session`|`tree`, default `tree`) is the only known key. */
export function parseViewQuery(query: Record<string, string>): ViewQuery {
  for (const key of Object.keys(query)) {
    if (key !== "scope") {
      return { ok: false, error: `unknown query parameter \`${key}\`` };
    }
  }
  const scope = query.scope ?? "tree";
  if (scope !== "session" && scope !== "tree") {
    return { ok: false, error: "`scope` must be `session` or `tree` for a view" };
  }
  return { ok: true, scope };
}

export type ToolInput =
  | { ok: true; scope: "session" | "tree" | "root"; format: "table" | "json" }
  | { ok: false; error: string };

/** Validate `token_summary` tool input: `scope` (default `tree`) and `format` (default `table`). */
export function parseToolInput(raw: unknown): ToolInput {
  if (raw !== undefined && raw !== null && (typeof raw !== "object" || Array.isArray(raw))) {
    return { ok: false, error: "input must be an object" };
  }
  const input = (raw ?? {}) as Record<string, unknown>;
  for (const key of Object.keys(input)) {
    if (key !== "scope" && key !== "format") {
      return { ok: false, error: `unknown input field \`${key}\`` };
    }
  }
  const scope = input.scope ?? "tree";
  if (scope !== "session" && scope !== "tree" && scope !== "root") {
    return { ok: false, error: "`scope` must be `session`, `tree`, or `root`" };
  }
  const format = input.format ?? "table";
  if (format !== "table" && format !== "json") {
    return { ok: false, error: "`format` must be `table` or `json`" };
  }
  return { ok: true, scope, format };
}

// ------------------------------------------------------------- protocol

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function logError(message: string): void {
  process.stderr.write(`[hya-extra/token-summary] ${message}\n`);
}

/** A JSON-RPC error reported back on a `host/capability` reply. */
export class CapabilityError extends Error {
  constructor(
    public readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

interface PendingCapability {
  resolve: (result: unknown) => void;
  reject: (error: Error) => void;
}

/**
 * The plugin->host side of `host/capability`: mints one numeric id per
 * outgoing request (a separate counter from whatever ids the host uses for
 * its own host->plugin requests, since each direction of a JSON-RPC 2.0
 * connection tracks replies independently) and resolves/rejects the matching
 * promise when a reply line arrives. `handleFrame` is the reader's dispatch
 * point: any line that is *not* itself a request (no `method` field) is a
 * candidate reply and is claimed here; a genuine host request/notification is
 * left untouched (`handleFrame` returns `false`) so the caller dispatches it
 * normally. This is what lets a host request (another `view/get`/`tool/call`)
 * arrive and be answered while a capability reply is still outstanding — a
 * naive reader that only ever awaited the *next* line in request order would
 * deadlock the moment the two interleave.
 */
export class CapabilityClient {
  private seq = 0;
  private readonly pending = new Map<string, PendingCapability>();

  constructor(private readonly write: (frame: Record<string, unknown>) => void) {}

  /** Call one `session.usage`-style host operation; resolves with the raw `result` value. */
  call(hostCapability: string, session: string, call: string, method: string, params: unknown): Promise<unknown> {
    this.seq += 1;
    const id = this.seq;
    return new Promise((resolve, reject) => {
      this.pending.set(String(id), { resolve, reject });
      this.write({
        jsonrpc: "2.0",
        id,
        method: "host/capability",
        params: { capability: hostCapability, session, call, method, params },
      });
    });
  }

  /**
   * Try to consume `message` as a reply to one of this client's own
   * outstanding calls. Returns `false` (and does nothing) for any
   * `method`-bearing frame — those are host requests/notifications, never
   * replies, whatever their `id`.
   */
  handleFrame(message: Record<string, unknown>): boolean {
    if ("method" in message) return false;
    const id = message.id;
    const key = typeof id === "number" ? String(id) : typeof id === "string" ? id : null;
    if (key === null) {
      logError(`capability reply with a non-numeric id: ${JSON.stringify(id)}`);
      return true;
    }
    const pending = this.pending.get(key);
    if (!pending) {
      logError(`capability reply for an unknown request id ${key}`);
      return true;
    }
    this.pending.delete(key);
    if ("error" in message && message.error !== undefined) {
      const error = message.error as { code?: number; message?: string };
      pending.reject(new CapabilityError(error.code ?? 0, error.message ?? "capability call failed"));
    } else {
      pending.resolve(message.result);
    }
    return true;
  }
}

async function fetchUsage(
  cap: CapabilityClient,
  hostCapability: string,
  session: string,
  call: string,
  scope: string,
): Promise<{ ok: true; report: WireSessionUsageReport } | { ok: false; error: string }> {
  try {
    const result = await cap.call(hostCapability, session, call, "session.usage", { scope });
    return { ok: true, report: result as WireSessionUsageReport };
  } catch (error) {
    return { ok: false, error: error instanceof CapabilityError ? error.message : describe(error) };
  }
}

async function handleViewGet(rawParams: unknown, cap: CapabilityClient): Promise<{ body: unknown }> {
  if (!isRecord(rawParams)) return { body: { error: "malformed view/get params" } };
  if (rawParams.view !== VIEW_ID) {
    return { body: { error: `unknown view \`${String(rawParams.view)}\`` } };
  }
  const query = isRecord(rawParams.query) ? (rawParams.query as Record<string, string>) : {};
  const parsed = parseViewQuery(query);
  if (!parsed.ok) return { body: { error: parsed.error } };
  const session = rawParams.session;
  const call = rawParams.call;
  const hostCapability = rawParams.host_capability;
  if (typeof session !== "string" || typeof call !== "string" || typeof hostCapability !== "string") {
    return { body: { error: "malformed view/get params" } };
  }
  const usage = await fetchUsage(cap, hostCapability, session, call, parsed.scope);
  if (!usage.ok) return { body: { error: usage.error } };
  return { body: buildView(usage.report) };
}

async function handleToolCall(rawParams: unknown, cap: CapabilityClient): Promise<{ ok: boolean; output: unknown }> {
  if (!isRecord(rawParams)) return { ok: false, output: "malformed tool/call params" };
  if (rawParams.tool !== TOOL_ID) {
    return { ok: false, output: `unknown tool \`${String(rawParams.tool)}\`` };
  }
  const session = rawParams.session;
  const call = rawParams.call;
  const hostCapability = rawParams.host_capability;
  if (typeof session !== "string" || typeof call !== "string" || typeof hostCapability !== "string") {
    return { ok: false, output: "token_summary requires a session-bound tool call" };
  }
  const parsed = parseToolInput(rawParams.input);
  if (!parsed.ok) return { ok: false, output: parsed.error };
  const usage = await fetchUsage(cap, hostCapability, session, call, parsed.scope);
  if (!usage.ok) return { ok: false, output: usage.error };
  const view = buildView(usage.report);
  if (parsed.format === "json") return { ok: true, output: view };
  return { ok: true, output: renderTable(view, parsed.scope) };
}

const INITIALIZE_RESULT = {
  protocol_version: 1,
  plugin: { id: PLUGIN_ID, version: PLUGIN_VERSION, kind: "bun" },
  hooks: [],
  tools: [
    {
      name: TOOL_ID,
      description:
        "Per-model token usage (input, cache creation, cache read, output split into thinking/visible) for the current session tree.",
      inputSchema: {
        type: "object",
        properties: {
          scope: { type: "string", enum: ["session", "tree", "root"], default: "tree" },
          format: { type: "string", enum: ["table", "json"], default: "table" },
        },
        additionalProperties: false,
      },
    },
  ],
  skills: [],
  views: [{ name: VIEW_ID, description: "Per-model token usage of the session tree" }],
};

// --- stdio JSON-RPC loop (skipped when this file is imported for tests) ---

function writeFrame(frame: Record<string, unknown>, then?: () => void): void {
  process.stdout.write(`${JSON.stringify(frame)}\n`, then);
}

async function handleRequest(message: Record<string, unknown>, cap: CapabilityClient): Promise<void> {
  const id = message.id;
  const hasId = typeof id === "number";
  const respond = (result: unknown, then?: () => void) => {
    if (hasId) writeFrame({ jsonrpc: "2.0", id, result }, then);
  };
  const method = message.method;
  try {
    switch (method) {
      case "initialize":
        respond(INITIALIZE_RESULT);
        return;
      case "shutdown":
        respond({}, () => process.exit(0));
        return;
      case "view/get": {
        const result = await handleViewGet(message.params, cap);
        respond(result);
        return;
      }
      case "tool/call": {
        const result = await handleToolCall(message.params, cap);
        respond(result);
        return;
      }
      default:
        // `event` and every other hook this process never registered: an
        // empty reply for any id-bearing method; notifications get none.
        respond({});
    }
  } catch (error) {
    logError(`request \`${String(method)}\` failed: ${describe(error)}`);
    respond({});
  }
}

async function main(): Promise<void> {
  const cap = new CapabilityClient(writeFrame);
  const rl = createInterface({ input: process.stdin, terminal: false });
  for await (const rawLine of rl) {
    const line = rawLine.trim();
    if (line.length === 0) continue;
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch {
      logError("ignoring a non-JSON line");
      continue;
    }
    if (!isRecord(message)) continue;
    if (cap.handleFrame(message)) continue;
    // Answer concurrently: a slow `view/get`/`tool/call` (or its capability
    // round trip) must never block reading the next line.
    void handleRequest(message, cap);
  }
}

if (import.meta.main) {
  main().catch((error) => {
    logError(`fatal: ${describe(error)}`);
    process.exit(1);
  });
}
