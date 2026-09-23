// hya-extra/jev-model-router: a `chat.params` plugin that asks Jev (TypeSafe
// System One, https://docs.typesafe.ai) how difficult a request is and rewrites
// `request.model` to the model of the matching tier. One decision is kept per
// request chain (or session) so a conversation stays on one provider/model and
// its prompt cache stays warm.
//
// hya spawns this file directly (`bun run router.ts`); no adapter is injected,
// so it speaks the hya plugin protocol v1 itself: newline-delimited JSON-RPC 2.0
// over stdio. stdout carries protocol frames only; diagnostics go to stderr.
//
// Everything is fail-open: a missing or invalid config, a Jev error or timeout,
// or any exception leaves requests routable (unchanged, or on `default_tier`).

import { readFileSync } from "node:fs";

/** Plugin id reported on initialize: the bundle namespace (identity name segment). */
export const PLUGIN_ID = "jev-model-router";
/** Version of this router script's protocol implementation. */
export const PLUGIN_VERSION = "1.0.0";
export const DEFAULT_ENDPOINT = "https://api.typesafe.ai/v1/systemone";
/** Stay well inside hya's 30 s hook-call timeout. */
export const MAX_TIMEOUT_MS = 20_000;
/** Bounded decision cache (keys = request chains or sessions). */
export const CACHE_KEYS = 1024;
const USER_TEXT_CHARS = 4000;
const SYSTEM_HEAD_CHARS = 1000;
const QUESTION = "difficulty";

export type Stickiness = "chain" | "session" | "none";

export interface Tier {
  name: string;
  model: string;
  criteria: string;
}

export interface RouterConfig {
  jev: {
    endpoint: string;
    model: string;
    apiKey: string;
    timeoutMs: number;
    minConfidence: number;
  };
  route: {
    from: string[];
    /** Index into `tiers`. */
    defaultTier: number;
    stickiness: Stickiness;
    escalate: boolean;
  };
  /** Ordered easy -> hard. */
  tiers: Tier[];
}

export type ConfigResult = { ok: true; config: RouterConfig } | { ok: false; error: string };

/** The `request` field of `hook/chat.params` (hya `WireCompletionRequest`). */
export interface WireRequest {
  model: string;
  system?: string;
  messages: unknown[];
  tools: unknown[];
  temperature?: number;
  max_output_tokens?: number;
  reasoning?: string;
  headers?: Record<string, string>;
  [field: string]: unknown;
}

export interface ChatParams {
  session: string;
  root_session?: string;
  agent?: string;
  message: string;
  request: WireRequest;
}

/** Resolves a request to a tier index, or `null` when Jev cannot decide. */
export type Classifier = (params: ChatParams) => Promise<number | null>;
export type Logger = (message: string) => void;

export const stderrLog: Logger = (message) => {
  process.stderr.write(`jev-model-router: ${message}\n`);
};

// ---------------------------------------------------------------- config

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function checkKeys(where: string, value: Record<string, unknown>, allowed: string[]): void {
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) throw new Error(`${where}: unknown key \`${key}\``);
  }
}

function nonEmptyString(where: string, value: unknown): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${where} must be a non-empty string`);
  }
  return value;
}

/**
 * Validate the bundle config (`config.yml` already parsed). Unknown top-level
 * keys are ignored (the same file holds hya's own `agents:` leaf); unknown keys
 * inside `jev`, `route` and tiers are rejected to catch typos.
 */
export function parseConfig(
  raw: unknown,
  readFile: (path: string) => string = (path) => readFileSync(path, "utf8"),
): ConfigResult {
  try {
    if (!isRecord(raw)) throw new Error("config must be a mapping");

    const jevRaw = raw.jev ?? {};
    if (!isRecord(jevRaw)) throw new Error("jev must be a mapping");
    checkKeys("jev", jevRaw, ["endpoint", "model", "api_key", "api_key_file", "timeout_ms", "min_confidence"]);
    const hasKey = jevRaw.api_key !== undefined;
    const hasKeyFile = jevRaw.api_key_file !== undefined;
    if (hasKey === hasKeyFile) throw new Error("set exactly one of jev.api_key or jev.api_key_file");
    let apiKey: string;
    if (hasKey) {
      apiKey = nonEmptyString("jev.api_key", jevRaw.api_key).trim();
    } else {
      const path = nonEmptyString("jev.api_key_file", jevRaw.api_key_file);
      if (!path.startsWith("/")) throw new Error("jev.api_key_file must be an absolute path");
      apiKey = nonEmptyString(`jev.api_key_file (${path})`, readFile(path)).trim();
    }
    const timeoutMs = jevRaw.timeout_ms ?? 2000;
    if (!Number.isInteger(timeoutMs) || (timeoutMs as number) <= 0 || (timeoutMs as number) > MAX_TIMEOUT_MS) {
      throw new Error(`jev.timeout_ms must be an integer in 1..=${MAX_TIMEOUT_MS}`);
    }
    const minConfidence = jevRaw.min_confidence ?? 0.5;
    if (typeof minConfidence !== "number" || minConfidence < 0 || minConfidence > 1) {
      throw new Error("jev.min_confidence must be a number in 0..=1");
    }

    if (!Array.isArray(raw.tiers) || raw.tiers.length === 0) {
      throw new Error("tiers must be a non-empty list ordered easy -> hard");
    }
    const tiers: Tier[] = raw.tiers.map((tier, index) => {
      const where = `tiers[${index}]`;
      if (!isRecord(tier)) throw new Error(`${where} must be a mapping`);
      checkKeys(where, tier, ["name", "model", "criteria"]);
      return {
        name: nonEmptyString(`${where}.name`, tier.name),
        model: nonEmptyString(`${where}.model`, tier.model),
        criteria: nonEmptyString(`${where}.criteria`, tier.criteria),
      };
    });
    const names = new Set(tiers.map((tier) => tier.name));
    if (names.size !== tiers.length) throw new Error("tier names must be unique");

    const routeRaw = raw.route ?? {};
    if (!isRecord(routeRaw)) throw new Error("route must be a mapping");
    checkKeys("route", routeRaw, ["from", "default_tier", "stickiness", "escalate"]);
    const from = routeRaw.from ?? [];
    if (!Array.isArray(from) || !from.every((model) => typeof model === "string" && model !== "")) {
      throw new Error("route.from must be a list of model refs");
    }
    const defaultName = routeRaw.default_tier ?? tiers[tiers.length - 1].name;
    const defaultTier = tiers.findIndex((tier) => tier.name === defaultName);
    if (defaultTier < 0) throw new Error(`route.default_tier \`${String(defaultName)}\` is not a tier name`);
    const stickiness = routeRaw.stickiness ?? "chain";
    if (stickiness !== "chain" && stickiness !== "session" && stickiness !== "none") {
      throw new Error("route.stickiness must be chain, session or none");
    }
    const escalate = routeRaw.escalate ?? false;
    if (typeof escalate !== "boolean") throw new Error("route.escalate must be a boolean");

    return {
      ok: true,
      config: {
        jev: {
          endpoint: jevRaw.endpoint === undefined ? DEFAULT_ENDPOINT : nonEmptyString("jev.endpoint", jevRaw.endpoint),
          model: jevRaw.model === undefined ? "jev-latest" : nonEmptyString("jev.model", jevRaw.model),
          apiKey,
          timeoutMs: timeoutMs as number,
          minConfidence,
        },
        route: { from: from as string[], defaultTier, stickiness, escalate },
        tiers,
      },
    };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
}

/** Read and validate `path` (normally `$HYA_BUNDLE_CONFIG_FILE`). */
export function loadConfig(path: string | undefined): ConfigResult {
  if (!path) return { ok: false, error: "HYA_BUNDLE_CONFIG_FILE is not set" };
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch {
    return { ok: false, error: `no config at ${path}` };
  }
  try {
    return parseConfig(Bun.YAML.parse(text));
  } catch (error) {
    return { ok: false, error: `${path}: ${error instanceof Error ? error.message : String(error)}` };
  }
}

// ---------------------------------------------------------------- request shape

/**
 * The newest `role: user` message: its id and text parts. Tool results live in
 * assistant tool parts, so a tool-result round never adds a user message; a new
 * id therefore means a new user turn (ids stay stable across compaction).
 */
export function latestUserMessage(messages: unknown[]): { id: string; text: string } | null {
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    if (!isRecord(message) || message.role !== "user") continue;
    const parts = Array.isArray(message.parts) ? message.parts : [];
    const text = parts
      .filter((part): part is Record<string, unknown> => isRecord(part) && part.type === "text")
      .map((part) => String(part.text ?? ""))
      .join("\n");
    return { id: String(message.id ?? index), text };
  }
  return null;
}

function truncate(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, max)}…`;
}

// ---------------------------------------------------------------- Jev wire

/** One `choice` question over tier names; state is a compact request summary. */
export function buildJevRequest(config: RouterConfig, params: ChatParams): unknown {
  const request = params.request;
  const latest = latestUserMessage(request.messages ?? []);
  return {
    model: config.jev.model,
    state: {
      agent: params.agent ?? null,
      latest_user_message: truncate(latest?.text ?? "", USER_TEXT_CHARS),
      system_prompt_head: truncate(request.system ?? "", SYSTEM_HEAD_CHARS),
      tool_count: Array.isArray(request.tools) ? request.tools.length : 0,
      message_count: Array.isArray(request.messages) ? request.messages.length : 0,
    },
    questions: {
      [QUESTION]: {
        type: "choice",
        instructions:
          "An AI coding agent (`agent`) received `latest_user_message`. How difficult is this task? " +
          "Pick the cheapest tier whose description still covers it; `system_prompt_head`, " +
          "`tool_count` and `message_count` describe the agent's context.",
        criteria: Object.fromEntries(config.tiers.map((tier) => [tier.name, tier.criteria])),
      },
    },
  };
}

/** Map a Jev response body to a tier index, or explain why it cannot be used. */
export function parseJevAnswer(
  body: unknown,
  tiers: Tier[],
  minConfidence: number,
): { tier: number } | { error: string } {
  const answer = isRecord(body) && isRecord(body.answers) ? body.answers[QUESTION] : undefined;
  if (!isRecord(answer) || answer.type !== "choice" || typeof answer.choice !== "string") {
    return { error: "response has no choice answer" };
  }
  const tier = tiers.findIndex((candidate) => candidate.name === answer.choice);
  if (tier < 0) return { error: `unknown choice \`${answer.choice}\`` };
  const confidence = typeof answer.confidence === "number" ? answer.confidence : 0;
  if (confidence < minConfidence) {
    return { error: `low confidence ${confidence} for \`${answer.choice}\`` };
  }
  return { tier };
}

/** A classifier that calls the Jev endpoint; every failure resolves to `null`. */
export function jevClassifier(
  config: RouterConfig,
  fetchImpl: typeof fetch = fetch,
  log: Logger = stderrLog,
): Classifier {
  return async (params) => {
    try {
      const response = await fetchImpl(config.jev.endpoint, {
        method: "POST",
        headers: {
          authorization: `Bearer ${config.jev.apiKey}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(buildJevRequest(config, params)),
        signal: AbortSignal.timeout(config.jev.timeoutMs),
      });
      if (!response.ok) {
        log(`Jev returned HTTP ${response.status}; using default_tier`);
        return null;
      }
      const parsed = parseJevAnswer(await response.json(), config.tiers, config.jev.minConfidence);
      if ("error" in parsed) {
        log(`${parsed.error}; using default_tier`);
        return null;
      }
      return parsed.tier;
    } catch (error) {
      log(`Jev call failed (${error instanceof Error ? error.message : String(error)}); using default_tier`);
      return null;
    }
  };
}

// ---------------------------------------------------------------- routing

/** Minimal insertion-ordered LRU. */
export class LruMap<K, V> {
  private readonly map = new Map<K, V>();
  constructor(private readonly capacity: number) {}

  get(key: K): V | undefined {
    const value = this.map.get(key);
    if (value !== undefined) {
      this.map.delete(key);
      this.map.set(key, value);
    }
    return value;
  }

  set(key: K, value: V): void {
    this.map.delete(key);
    this.map.set(key, value);
    if (this.map.size > this.capacity) {
      const oldest = this.map.keys().next();
      if (!oldest.done) this.map.delete(oldest.value);
    }
  }
}

interface Decision {
  tier: number;
  /** Id of the newest user message the key's owner session has been routed on. */
  lastUser?: string;
}

export class Router {
  private readonly decisions = new LruMap<string, Promise<Decision>>(CACHE_KEYS);

  constructor(
    private readonly config: RouterConfig | null,
    private readonly classify: Classifier,
    private readonly log: Logger = stderrLog,
  ) {}

  /** The request to send: `params.request` with `model` possibly rewritten. Never throws. */
  async route(params: ChatParams): Promise<WireRequest> {
    const request = params.request;
    try {
      const config = this.config;
      if (!config) return request;
      if (config.route.from.length > 0 && !config.route.from.includes(request.model)) return request;
      const tier = await this.decide(config, params);
      return { ...request, model: config.tiers[tier].model };
    } catch (error) {
      this.log(`routing failed (${error instanceof Error ? error.message : String(error)}); request unchanged`);
      return request;
    }
  }

  private async decide(config: RouterConfig, params: ChatParams): Promise<number> {
    const { stickiness, escalate, defaultTier } = config.route;
    const latest = latestUserMessage(params.request.messages ?? []);
    const key =
      stickiness === "chain"
        ? params.root_session ?? params.session
        : stickiness === "session"
          ? params.session
          : null;
    if (key === null) return (await this.classify(params)) ?? defaultTier;

    // Only the key's owner (the chain root, or the session itself) advances the
    // "last user message" marker; subagent prompts never trigger escalation.
    const owner = stickiness === "session" || key === params.session;
    const cached = this.decisions.get(key);
    if (cached === undefined) {
      const pending = this.classify(params).then((tier) => ({
        tier: tier ?? defaultTier,
        lastUser: owner ? latest?.id : undefined,
      }));
      // Concurrent first calls for one key share this classification.
      this.decisions.set(key, pending);
      try {
        return (await pending).tier;
      } catch (error) {
        this.decisions.set(key, Promise.resolve({ tier: defaultTier, lastUser: owner ? latest?.id : undefined }));
        throw error;
      }
    }

    const decision = await cached;
    if (!owner || latest === null || latest.id === decision.lastUser) return decision.tier;
    decision.lastUser = latest.id;
    if (!escalate) return decision.tier;
    const verdict = await this.classify(params);
    if (verdict !== null && verdict > decision.tier) {
      this.log(`escalating ${key} from ${config.tiers[decision.tier].name} to ${config.tiers[verdict].name}`);
      decision.tier = verdict;
    }
    return decision.tier;
  }
}

// ---------------------------------------------------------------- protocol

export interface RpcMessage {
  jsonrpc?: string;
  id?: number | string | null;
  method?: string;
  params?: unknown;
}

export interface RpcReply {
  jsonrpc: "2.0";
  id: number | string | null;
  result: unknown;
}

/** Answer one host message; `null` for notifications (no `id`). */
export async function handleMessage(message: RpcMessage, router: Router): Promise<RpcReply | null> {
  if (message.id === undefined || message.id === null) return null;
  const reply = (result: unknown): RpcReply => ({ jsonrpc: "2.0", id: message.id as number | string, result });
  switch (message.method) {
    case "initialize":
      return reply({
        protocol_version: 1,
        plugin: { id: PLUGIN_ID, version: PLUGIN_VERSION, kind: "bun" },
        tools: [],
        hooks: [{ name: "chat.params", posture: "open" }],
        skills: [],
      });
    case "hook/chat.params": {
      const params = message.params as ChatParams;
      if (!isRecord(params) || !isRecord(params.request)) return reply({ outcome: "continue", request: params?.request });
      return reply({ outcome: "continue", request: await router.route(params) });
    }
    default:
      return reply({});
  }
}

async function main(): Promise<void> {
  const loaded = loadConfig(process.env.HYA_BUNDLE_CONFIG_FILE);
  let router: Router;
  if (loaded.ok) {
    router = new Router(loaded.config, jevClassifier(loaded.config));
    stderrLog(`routing across tiers ${loaded.config.tiers.map((tier) => tier.name).join(", ")}`);
  } else {
    // Never fail initialize: a misconfigured router must not wedge the runtime.
    stderrLog(`not configured (${loaded.error}); passing every request through unchanged`);
    router = new Router(null, async () => null);
  }

  const write = (frame: RpcReply, then?: () => void) => {
    process.stdout.write(`${JSON.stringify(frame)}\n`, then);
  };
  const decoder = new TextDecoder();
  let buffer = "";
  for await (const chunk of Bun.stdin.stream()) {
    buffer += decoder.decode(chunk, { stream: true });
    let newline: number;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (line === "") continue;
      let message: RpcMessage;
      try {
        message = JSON.parse(line);
      } catch {
        stderrLog("ignoring a non-JSON line");
        continue;
      }
      // Hooks for different sessions may overlap: answer each as it completes.
      void handleMessage(message, router)
        .then((frame) => {
          if (!frame) return;
          const exit = message.method === "shutdown" ? () => process.exit(0) : undefined;
          write(frame, exit);
        })
        .catch((error) => stderrLog(`unhandled: ${String(error)}`));
    }
  }
}

if (import.meta.main) {
  await main();
}
