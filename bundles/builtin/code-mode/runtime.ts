// Self-contained hya plugin protocol v1 process; stdout is NDJSON only.
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { createInterface } from "node:readline";
import vm from "node:vm";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

export interface ToolSchema { name: string; description: string; input_schema: Record<string, unknown>; }
export const PERSONALITIES = {
  workflow: { name: "workflow", plugin: "orchestrator", label: "Workflow", file: "orchestrator-script.js", tools: false },
  run: { name: "run", plugin: "code-mode", label: "Script", file: "code-mode-script.js", tools: true },
};
type Personality = typeof PERSONALITIES[keyof typeof PERSONALITIES];
export type Host = (method: string, params: Record<string, unknown>, signal: AbortSignal) => Promise<any>;
export interface Options {
  timeout_ms: number;
  max_output_chars: number;
}
export interface JudgeConfig {
  endpoint: string;
  model: string;
  apiKey: string;
  timeoutMs: number;
}
export type Question =
  | { type: "bool"; instructions: string; criteria?: { true: string; false: string } }
  | { type: "choice"; instructions: string; criteria: Record<string, string> }
  | { type: "score"; instructions: string; criteria: string[] };
export interface ClassifyInput {
  state: unknown;
  questions: Record<string, Question>;
}
export interface AgentInput {
  agent: string;
  prompt: string;
  description?: string;
  result?: Record<string, string>;
  model?: string;
  effort?: string;
  timeout_secs?: number;
  cache?: boolean;
}
export interface AgentResult {
  handle: string;
  session: string;
  status: "done" | "failed" | "cancelled" | "timeout";
  report: string;
  result: Record<string, unknown> | null;
  cached?: true;
}
export interface TraceEntry {
  seq: number;
  line: number;
  kind: "agent" | "classify" | "tool";
  name: string;
  input: string;
  ok: boolean;
  output?: string;
  error?: string;
  started_ms: number;
  ms: number;
  handle?: string;
  session?: string;
  result?: Record<string, unknown> | null;
  state_before?: string | null;
  state_after?: string | null;
  cached?: true;
  source?: { handle: string; session: string; trace: string; seq: number };
  abandoned?: boolean;
  archived?: boolean;
  archive_error?: string;
}
export const DEFAULT_OPTIONS: Options = { timeout_ms: 1_800_000, max_output_chars: 3000 };
const MAX_RESULT_CHARS = 4799;

function record(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function message(error: unknown): string {
  // Errors thrown inside the VM belong to a different realm.
  return record(error) && typeof error.message === "string" ? error.message : String(error);
}
function string(value: unknown, field: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} must be a non-empty string`);
  return value;
}
function integer(value: unknown, max: number, field: string): number {
  if (!Number.isInteger(value) || (value as number) < 1 || (value as number) > max) {
    throw new Error(`${field} must be an integer in 1..=${max}`);
  }
  return value as number;
}
function json(value: unknown): string {
  return JSON.stringify(value) ?? "undefined";
}
export function truncate(value: string, max: number): string {
  if (value.length <= max) return value;
  const marker = "\n… [truncated] …\n";
  if (max <= marker.length) return marker.slice(0, max);
  const head = Math.ceil((max - marker.length) / 2);
  const tail = max - marker.length - head;
  return value.slice(0, head) + marker + (tail ? value.slice(-tail) : "");
}

export function parseOptions(script: string): Options {
  const lines = script.split("\n");
  const matches = lines.map((line, index) => /^\s*\/\/\s*@options\s*:/.test(line) ? index : -1).filter((i) => i >= 0);
  if (matches.some((i) => i !== 0)) throw new Error("@options must appear only on the first line");
  if (!matches.length) return { ...DEFAULT_OPTIONS };
  const raw = JSON.parse(lines[0].replace(/^\s*\/\/\s*@options\s*:/, ""));
  if (!record(raw)) throw new Error("@options must be a JSON object");
  for (const key of Object.keys(raw)) {
    if (key !== "timeout_ms" && key !== "max_output_chars") throw new Error(`unknown @options key: ${key}`);
  }
  return {
    timeout_ms: integer(raw.timeout_ms === undefined ? DEFAULT_OPTIONS.timeout_ms : raw.timeout_ms, 3_000_000, "timeout_ms"),
    max_output_chars: integer(raw.max_output_chars === undefined ? DEFAULT_OPTIONS.max_output_chars : raw.max_output_chars, 3600, "max_output_chars"),
  };
}

export function parseJudgeConfig(raw: unknown, readFile = (path: string) => readFileSync(path, "utf8")): JudgeConfig | null {
  if (!record(raw)) throw new Error("config must be a mapping");
  if (raw.judge === undefined) return null;
  const judge = raw.judge;
  if (!record(judge)) throw new Error("judge must be a mapping");
  for (const key of Object.keys(judge)) {
    if (!["endpoint", "model", "api_key", "api_key_file", "timeout_ms"].includes(key)) {
      throw new Error(`unknown judge key: ${key}`);
    }
  }
  if ((judge.api_key !== undefined) === (judge.api_key_file !== undefined)) {
    throw new Error("set exactly one of judge.api_key or judge.api_key_file");
  }
  let key = judge.api_key;
  if (judge.api_key_file !== undefined) {
    const path = string(judge.api_key_file, "judge.api_key_file");
    if (!path.startsWith("/")) throw new Error("judge.api_key_file must be an absolute path");
    key = readFile(path);
  }
  return {
    endpoint: string(judge.endpoint === undefined ? "https://api.typesafe.ai/v1/systemone" : judge.endpoint, "judge.endpoint"),
    model: string(judge.model === undefined ? "jev-1.14.0" : judge.model, "judge.model"),
    apiKey: string(key, "judge.api_key").trim(),
    timeoutMs: integer(judge.timeout_ms === undefined ? 20_000 : judge.timeout_ms, 3_000_000, "judge.timeout_ms"),
  };
}

export function buildJevRequest(config: JudgeConfig, input: ClassifyInput): Record<string, unknown> {
  if (!record(input) || !record(input.questions) || !Object.keys(input.questions).length || input.state === undefined) {
    throw new Error("models.classify requires state and a non-empty questions mapping");
  }
  const questions = Object.fromEntries(Object.entries(input.questions).map(([id, question]) => {
    if (!record(question)) throw new Error(`question ${id} must be an object`);
    string(question.instructions, `question ${id}.instructions`);
    switch (question.type) {
      case "bool":
        if (question.criteria !== undefined) {
          if (!record(question.criteria)) throw new Error(`question ${id}.criteria must map true and false to descriptions`);
          string(question.criteria.true, `question ${id}.criteria.true`);
          string(question.criteria.false, `question ${id}.criteria.false`);
        }
        return [id, { ...question, type: "noul" }];
      case "choice":
        if (!record(question.criteria) || !Object.keys(question.criteria).length) throw new Error(`question ${id}.criteria must be a label mapping`);
        for (const text of Object.values(question.criteria)) string(text, `question ${id}.criteria`);
        return [id, question];
      case "score":
        if (!Array.isArray(question.criteria) || !question.criteria.length) throw new Error(`question ${id}.criteria must be a non-empty array`);
        for (const text of question.criteria) string(text, `question ${id}.criteria`);
        return [id, question];
      default:
        throw new Error(`question ${id}: unknown type ${question.type}`);
    }
  }));
  return { model: config.model, state: input.state, questions };
}

function probability(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) throw new Error(`invalid Jev ${field}`);
  return value;
}
export function parseJevAnswers(body: unknown, questions: Record<string, Question>): { answers: Record<string, unknown> } {
  if (!record(body) || !record(body.answers)) throw new Error("Jev response has no answers mapping");
  const answers = Object.fromEntries(Object.entries(questions).map(([id, question]) => {
    const answer = body.answers[id];
    if (!record(answer)) throw new Error(`Jev response has no answer for ${id}`);
    switch (question.type) {
      case "bool":
        if (answer.type !== "noul") throw new Error(`Jev answer ${id} must be noul`);
        return [id, { type: "bool", probability: probability(answer.noul, `${id}.noul`) }];
      case "choice":
        if (answer.type !== "choice" || typeof answer.choice !== "string" || !Object.hasOwn(question.criteria, answer.choice) || !record(answer.probabilities)) {
          throw new Error(`invalid Jev choice answer ${id}`);
        }
        for (const value of Object.values(answer.probabilities)) probability(value, `${id}.probabilities`);
        return [id, { type: "choice", choice: answer.choice, probabilities: answer.probabilities, confidence: probability(answer.confidence, `${id}.confidence`) }];
      case "score":
        if (answer.type !== "score" || typeof answer.score !== "number" || !Number.isFinite(answer.score) || answer.score < 0 || answer.score > question.criteria.length - 1) {
          throw new Error(`invalid Jev score answer ${id}`);
        }
        return [id, { type: "score", score: answer.score, confidence: probability(answer.confidence, `${id}.confidence`) }];
    }
  }));
  return { answers };
}
export async function classify(config: JudgeConfig | null, input: ClassifyInput, signal: AbortSignal, fetchImpl: typeof fetch = fetch) {
  if (!config) throw new Error("No judge configured; set judge.api_key or judge.api_key_file in the bundle config.yml");
  const response = await fetchImpl(config.endpoint, {
    method: "POST",
    headers: { authorization: `Bearer ${config.apiKey}`, "content-type": "application/json" },
    body: JSON.stringify(buildJevRequest(config, input)),
    signal: AbortSignal.any([signal, AbortSignal.timeout(config.timeoutMs)]),
  });
  if (!response.ok) throw new Error(`Jev request failed: HTTP ${response.status}`);
  return parseJevAnswers(await response.json(), input.questions);
}

const WORKER_AGENTS = ["orchestrator-scout", "orchestrator-runner", "orchestrator-editor"];

export async function callHostTool(host: Host, name: string, input: Record<string, unknown>, signal: AbortSignal): Promise<any> {
  if (!["task", "wait", "archive", "read"].includes(name)
    || (name === "read" && (typeof input.path !== "string" || !input.path.startsWith("channel://")))) {
    throw new Error("A workflow can only call agents.run and models.classify; host calls are limited to worker coordination and channel reads");
  }
  return (await host("tool.call", { name, input }, signal)).output;
}

function parseAgentResult(report: string, requested?: Record<string, string>): Record<string, unknown> | null {
  const last = report.split("\n").map((line) => line.replace(/^\[[^\]\r\n]+\]\s*/, "")).filter((line) => line.startsWith("RESULT:")).at(-1);
  if (last === undefined) return null;
  let value: unknown;
  try { value = JSON.parse(last.slice("RESULT:".length).trim()); }
  catch { return null; }
  return record(value) && Object.keys(requested ?? {}).every((key) => Object.hasOwn(value, key)) ? value : null;
}

function prepareAgent(input: AgentInput, personality = PERSONALITIES.workflow) {
  if (!record(input)) throw new Error("agents.run requires an argument object");
  string(input.prompt, "agents.run prompt");
  const agent = string(input.agent, "agents.run agent");
  if (!personality.tools && !WORKER_AGENTS.includes(agent)) throw new Error(`agents.run agent must be one of: ${WORKER_AGENTS.join(", ")}`);
  let prompt = input.prompt;
  if (input.result !== undefined) {
    if (!record(input.result)) throw new Error("agents.run result must map field names to descriptions");
    for (const description of Object.values(input.result)) string(description, "agents.run result description");
    prompt += "\n\nEnd your report with one final line of the form RESULT: {...} — a single-line JSON object with exactly these fields:"
      + Object.entries(input.result).map(([field, description]) => `\n- ${field}: ${description}`).join("");
  }
  const timeout = input.timeout_secs === undefined ? 600 : input.timeout_secs;
  if (typeof timeout !== "number" || !Number.isFinite(timeout) || timeout < 0 || timeout > 1800) {
    throw new Error("agents.run timeout_secs must be in 0..=1800");
  }
  if (input.cache !== undefined && typeof input.cache !== "boolean") throw new Error("agents.run cache must be a boolean");
  return {
    timeout,
    task: {
      subagent_type: agent,
      prompt,
      description: input.description ?? input.prompt.slice(0, 80),
      ...(input.model === undefined ? {} : { model: input.model }),
      ...(input.effort === undefined ? {} : { effort: input.effort }),
    },
  };
}

export async function runAgent(input: AgentInput, host: Host, signal: AbortSignal, onStarted?: (result: AgentResult) => void, startSignal = signal, personality = PERSONALITIES.workflow): Promise<AgentResult> {
  const prepared = prepareAgent(input, personality);
  const call = (name: string, args: Record<string, unknown>) => callHostTool(host, name, args, signal);
  const task = await callHostTool(host, "task", { tasks: [prepared.task] }, startSignal);
  startSignal.throwIfAborted();
  const member = task.metadata.members[0];
  const result: AgentResult = { handle: member.member, session: member.session, status: "timeout", report: "", result: null };
  onStarted?.(result);
  if (member.status !== "running") {
    result.status = member.status === "done" || member.status === "completed" ? "done" : member.status === "cancelled" ? "cancelled" : "failed";
    result.report = member.summary;
    result.result = parseAgentResult(result.report, input.result);
    return result;
  }
  const deadline = Date.now() + prepared.timeout * 1000;
  for (;;) {
    signal.throwIfAborted();
    const wait = await call("wait", { targets: [result.handle], mode: "all", timeout_secs: Math.max(0, Math.ceil((deadline - Date.now()) / 1000)) });
    const metadata = wait.metadata;
    const terminal = [...metadata.finished, ...(metadata.already_finished ?? [])].find((entry) => entry.handle === result.handle);
    if (terminal) {
      result.status = terminal.outcome === "done" ? "done" : terminal.outcome === "failed" ? "failed" : "cancelled";
      const read = await call("read", { path: `channel://${terminal.channel ?? result.handle}` });
      // Channel reads expose rendered text, not a structured report body.
      result.report = read.output;
      result.result = parseAgentResult(result.report, input.result);
      return result;
    }
    if (metadata.running.some((entry) => entry.handle === result.handle && entry.state === "stopped_by_user")) {
      result.status = "cancelled";
      return result;
    }
    if (metadata.woke_by === "timeout" || Date.now() >= deadline) {
      // Nothing can reach a worker after its call returned, so it must not keep running.
      await call("archive", { target: result.handle, reason: "agents.run timed out" });
      return result;
    }
    // Mail/stall wakes are not terminal; wait again within the same deadline.
  }
}

function toolIdentifier(name: string): string {
  return name.replace(/[^\p{ID_Continue}$\u200C\u200D]/gu, "_").replace(/^[^\p{ID_Start}_$]/u, "_");
}
function scriptLine(): number {
  const match = new Error().stack?.match(/(?:orchestrator|code-mode)-script\.js:(\d+):\d+/);
  if (!match) throw new Error("could not locate the script call site");
  return Number(match[1]);
}
// The script starts on line 1 of the wrapper, so wrapper lines are script lines.
const SCRIPT_PREFIX = "(async function() {";
function wrapScript(script: string): string {
  return `${SCRIPT_PREFIX}${script}\n})()`;
}
function located(error: unknown): string {
  const line = record(error) && typeof error.stack === "string"
    ? error.stack.match(/(?:orchestrator|code-mode)-script\.js:(\d+)/)?.[1] : undefined;
  return line ? `${message(error)} (line ${line})` : message(error);
}

// Pre-run check: a script that would fail on syntax, an unknown tool, or a
// wrong argument object is rejected before it makes any call.
export interface Diagnostic { line: number; column: number | null; message: string }
const transpiler = new Bun.Transpiler({ loader: "js" });
function syntaxDiagnostics(script: string): Diagnostic[] {
  const wrapped = wrapScript(script);
  try {
    new vm.Script(wrapped, { filename: "code-mode-script.js" });
    return [];
  } catch (error) {
    // JSC runs the script, so it decides validity; Bun's parser locates and explains the error.
    const line = record(error) && typeof error.stack === "string" ? error.stack.match(/code-mode-script\.js:(\d+)/)?.[1] : undefined;
    const jsc: Diagnostic = { line: Number(line ?? 1), column: null, message: message(error) };
    try {
      transpiler.transformSync(wrapped);
    } catch (explained) {
      const errors: any[] = record(explained) && Array.isArray(explained.errors) ? explained.errors : [explained];
      // Later errors are usually cascades of the first; Bun columns are 1-based.
      const first = errors.find((entry) => record(entry?.position));
      if (first) {
        const { line, column } = first.position;
        return [{ line, column: Math.max(0, column - 1 - (line === 1 ? SCRIPT_PREFIX.length : 0)), message: first.message }];
      }
    }
    return [jsc];
  }
}

interface Token { kind: "id" | "punct" | "string" | "other"; value: string; line: number; column: number }
const idStart = (code: number) => (code >= 97 && code <= 122) || (code >= 65 && code <= 90) || code === 95 || code === 36 || code > 127;
const idPart = (code: number) => idStart(code) || (code >= 48 && code <= 57);
const REGEX_AFTER_WORD = new Set(["return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "await", "yield"]);
// Tokens of a script that JSC has accepted: enough to see calls and the keys of object literals.
function tokenize(source: string): Token[] {
  const tokens: Token[] = [];
  const templateBraces: number[] = [];
  let index = 0, line = 1, lineStart = 0;
  const push = (kind: Token["kind"], value: string, start: number, startLine = line, startLineStart = lineStart) =>
    tokens.push({ kind, value, line: startLine, column: start - startLineStart });
  const newline = (at: number) => { line++; lineStart = at + 1; };
  const skipEscape = () => { if (source[index + 1] === "\n") newline(index + 1); index += 2; };
  const template = (start: number) => {
    const startLine = line, startLineStart = lineStart;
    while (index < source.length) {
      const char = source[index];
      if (char === "\\") { skipEscape(); continue; }
      if (char === "`") { index++; break; }
      if (char === "$" && source[index + 1] === "{") { index += 2; templateBraces.push(0); break; }
      if (char === "\n") newline(index);
      index++;
    }
    push("other", "`", start, startLine, startLineStart);
  };
  while (index < source.length) {
    const char = source[index], code = source.charCodeAt(index), start = index;
    if (char === "\n") { newline(index); index++; continue; }
    if (code <= 32) { index++; continue; }
    const next = source[index + 1];
    if (char === "/" && next === "/") { while (index < source.length && source[index] !== "\n") index++; continue; }
    if (char === "/" && next === "*") {
      index += 2;
      while (index < source.length && !(source[index] === "*" && source[index + 1] === "/")) { if (source[index] === "\n") newline(index); index++; }
      index += 2;
      continue;
    }
    if (idStart(code)) {
      while (index < source.length && idPart(source.charCodeAt(index))) index++;
      push("id", source.slice(start, index), start);
      continue;
    }
    if (code >= 48 && code <= 57) {
      while (index < source.length && (idPart(source.charCodeAt(index)) || source[index] === ".")) index++;
      push("other", "0", start);
      continue;
    }
    if (char === '"' || char === "'") {
      index++;
      while (index < source.length && source[index] !== char) { if (source[index] === "\\") skipEscape(); else index++; }
      index++;
      push("string", source.slice(start + 1, index - 1), start);
      continue;
    }
    if (char === "`") { index++; template(start); continue; }
    if (char === "/") {
      const previous = tokens.at(-1);
      const regex = !previous || (previous.kind === "punct" && !")]}".includes(previous.value)) || (previous.kind === "id" && REGEX_AFTER_WORD.has(previous.value));
      if (regex) {
        index++;
        let inClass = false;
        while (index < source.length && source[index] !== "\n" && (inClass || source[index] !== "/")) {
          if (source[index] === "\\") index++;
          else if (source[index] === "[") inClass = true;
          else if (source[index] === "]") inClass = false;
          index++;
        }
        index++;
        while (index < source.length && idPart(source.charCodeAt(index))) index++;
        push("other", "/", start);
        continue;
      }
    }
    if (templateBraces.length && char === "{") templateBraces[templateBraces.length - 1]++;
    if (templateBraces.length && char === "}" && templateBraces[templateBraces.length - 1]-- === 0) {
      templateBraces.pop();
      index++;
      template(start);
      continue;
    }
    if (char === "." && next === "." && source[index + 2] === ".") { index += 3; push("punct", "...", start); continue; }
    if (char === "=" && next === ">") { index += 2; push("punct", "=>", start); continue; }
    index++;
    push("punct", char, start);
  }
  return tokens;
}

const OPENERS: Record<string, string> = { "(": ")", "[": "]", "{": "}" };
function closing(tokens: Token[], open: number): number {
  let depth = 0;
  for (let index = open; index < tokens.length; index++) {
    const { kind, value } = tokens[index];
    if (kind !== "punct") continue;
    if (OPENERS[value]) depth++;
    else if (value === ")" || value === "]" || value === "}") { if (--depth === 0) return index; }
  }
  return tokens.length - 1;
}
function opening(tokens: Token[], close: number): number {
  let depth = 0;
  for (let index = close; index >= 0; index--) {
    const { kind, value } = tokens[index];
    if (kind !== "punct") continue;
    if (value === ")" || value === "]" || value === "}") depth++;
    else if (OPENERS[value] && --depth === 0) return index;
  }
  return 0;
}
// Every name the script binds anywhere; over-collecting only suppresses diagnostics.
function boundNames(tokens: Token[]): Set<string> {
  const names = new Set<string>();
  const collect = (from: number, to: number) => {
    for (let index = from; index <= to; index++) if (tokens[index].kind === "id") names.add(tokens[index].value);
  };
  tokens.forEach((token, index) => {
    const next = tokens[index + 1];
    if (token.kind === "id" && /^(const|let|var|function|class|catch)$/.test(token.value) && next) {
      if (next.kind === "id") names.add(next.value);
      if (next.kind === "punct" && OPENERS[next.value]) collect(index + 1, closing(tokens, index + 1));
      if (token.value === "function") {
        const params = next.kind === "id" ? index + 2 : index + 1;
        if (tokens[params]?.value === "(") collect(params, closing(tokens, params));
      }
    }
    if (token.kind === "punct" && token.value === "=>" && index > 0) {
      const previous = tokens[index - 1];
      if (previous.kind === "id") names.add(previous.value);
      else if (previous.value === ")") collect(opening(tokens, index - 1), index - 1);
    }
  });
  return names;
}

interface ObjectLiteral { keys: string[]; open: boolean; values: Map<string, Token> }
// Top-level keys of the object literal at `start`; `open` when a spread or computed key hides some.
function objectLiteral(tokens: Token[], start: number): ObjectLiteral {
  const end = closing(tokens, start);
  const literal: ObjectLiteral = { keys: [], open: false, values: new Map() };
  let index = start + 1;
  while (index < end) {
    const token = tokens[index], after = tokens[index + 1];
    if (token.value === "..." || token.value === "[") literal.open = true;
    else if ((token.kind === "id" || token.kind === "string") && [":", ",", "}", "("].includes(after?.value ?? "")) {
      literal.keys.push(token.value);
      const value = tokens[index + 2];
      if (after.value === ":" && value?.kind === "string" && [",", "}"].includes(tokens[index + 3]?.value ?? "")) literal.values.set(token.value, value);
    }
    // Skip to the next top-level comma.
    while (index < end && tokens[index].value !== ",") index = OPENERS[tokens[index].value] && tokens[index].kind === "punct" ? closing(tokens, index) + 1 : index + 1;
    index++;
  }
  return literal;
}

function distance(a: string, b: string): number {
  const row = Array.from({ length: b.length + 1 }, (_, column) => column);
  for (let i = 1; i <= a.length; i++) {
    let diagonal = row[0];
    row[0] = i;
    for (let j = 1; j <= b.length; j++) {
      const above = row[j];
      row[j] = Math.min(row[j] + 1, row[j - 1] + 1, diagonal + (a[i - 1] === b[j - 1] ? 0 : 1));
      diagonal = above;
    }
  }
  return row[b.length];
}
function nearest(name: string, candidates: string[]): string[] {
  return candidates.map((candidate) => [candidate, distance(name, candidate)] as const)
    .sort((a, b) => a[1] - b[1]).slice(0, 3).map(([candidate]) => candidate);
}

const UNAVAILABLE = new Set(["require", "import", "process", "Bun", "fetch", "setTimeout", "setInterval", "setImmediate", "Buffer", "module", "exports", "__dirname", "__filename"]);
const AGENT_KEYS = ["agent", "prompt", "description", "result", "model", "effort", "timeout_secs", "cache"];
interface CallShape { keys: string[]; required: string[]; closed: boolean }
function argumentDiagnostics(call: string, shape: CallShape, literal: ObjectLiteral, at: Token): Diagnostic[] {
  const found: Diagnostic[] = [];
  const unknown = literal.keys.filter((key) => !shape.keys.includes(key));
  if (shape.closed) for (const key of unknown) {
    found.push({ line: at.line, column: at.column, message: `${call}: unknown argument \`${key}\`; it takes ${shape.keys.join(", ") || "no arguments"}` });
  }
  if (!literal.open) for (const key of shape.required.filter((key) => !literal.keys.includes(key))) {
    const given = !shape.closed && unknown.length ? ` (given: ${unknown.join(", ")})` : "";
    found.push({ line: at.line, column: at.column, message: `${call}: missing required argument \`${key}\`${given}` });
  }
  return found;
}

export function checkScript(script: string, schemas: ToolSchema[], personality: Personality): Diagnostic[] {
  const syntax = syntaxDiagnostics(script);
  if (syntax.length) return syntax;
  const tokens = tokenize(script);
  const bound = boundNames(tokens);
  const tools = new Map(schemas.map((schema) => [toolIdentifier(schema.name), schema]));
  const found: Diagnostic[] = [];
  const at = (token: Token, text: string) => found.push({ line: token.line, column: token.column, message: text });
  const literalArgument = (index: number) => tokens[index]?.value === "(" && tokens[index + 1]?.value === "{" ? objectLiteral(tokens, index + 1) : null;
  tokens.forEach((token, index) => {
    if (token.kind !== "id") return;
    const previous = tokens[index - 1], next = tokens[index + 1];
    if (previous?.value === "." && previous.kind === "punct") return;
    if (next?.value === ":" && (previous?.value === "{" || previous?.value === ",")) return;
    if (UNAVAILABLE.has(token.value) && !bound.has(token.value) && previous?.value !== "typeof") {
      at(token, `\`${token.value}\` is not available: a script has no modules, process, network or timers, and reaches the outside only through ${personality.tools ? "tools.*, " : ""}agents.run and models.classify`);
      return;
    }
    if (token.value === "tools" && !bound.has("tools")) {
      if (!personality.tools) { at(token, "A workflow can only call agents.run and models.classify; `tools` is not available"); return; }
      const name = tokens[index + 2];
      if (next?.value !== "." || name?.kind !== "id") return;
      const schema = tools.get(name.value);
      if (!schema) {
        at(name, `tools.${name.value} does not exist; nearest callable tools: ${nearest(name.value, [...tools.keys()]).map((tool) => `tools.${tool}`).join(", ") || "(none)"}`);
        return;
      }
      const literal = literalArgument(index + 3);
      const inputSchema: Record<string, any> = schema.input_schema;
      if (literal) found.push(...argumentDiagnostics(`tools.${name.value}`, {
        keys: Object.keys(record(inputSchema.properties) ? inputSchema.properties : {}),
        required: Array.isArray(inputSchema.required) ? inputSchema.required : [],
        closed: inputSchema.additionalProperties === false,
      }, literal, name));
      return;
    }
    const method = tokens[index + 2];
    if (next?.value !== "." || method?.kind !== "id") return;
    const call = `${token.value}.${method.value}`;
    if (call === "agents.run" && !bound.has("agents")) {
      const literal = literalArgument(index + 3);
      if (!literal) return;
      found.push(...argumentDiagnostics(call, { keys: AGENT_KEYS, required: ["agent", "prompt"], closed: true }, literal, method));
      const agent = literal.values.get("agent");
      if (agent && !personality.tools && !WORKER_AGENTS.includes(agent.value)) at(agent, `agents.run agent must be one of: ${WORKER_AGENTS.join(", ")}`);
    }
    if (call === "models.classify" && !bound.has("models")) {
      const literal = literalArgument(index + 3);
      if (literal) found.push(...argumentDiagnostics(call, { keys: ["state", "questions"], required: ["state", "questions"], closed: true }, literal, method));
    }
  });
  return found;
}

const SHOWN_DIAGNOSTICS = 5;
export function renderDiagnostics(script: string, diagnostics: Diagnostic[]): string {
  const lines = script.split("\n");
  const shown = diagnostics.slice(0, SHOWN_DIAGNOSTICS).map(({ line, column, message: text }) => {
    const source = lines[line - 1] ?? "";
    return column === null
      ? `L${line}: ${text}\n  ${source}`
      : `L${line}:${column + 1}: ${text}\n  ${source}\n  ${" ".repeat(column)}^`;
  });
  const more = diagnostics.length > SHOWN_DIAGNOSTICS ? `\n… ${diagnostics.length - SHOWN_DIAGNOSTICS} more` : "";
  return `the script was checked before running and made no call; fix it and send it again:\n${shown.join("\n")}${more}`;
}
function traceLine(entry: TraceEntry): string {
  const elapsed = entry.cached ? "0.0 s" : `${(entry.ms / 1000).toFixed(1)} s`;
  let call: string;
  let result: string;
  switch (entry.kind) {
    case "tool":
      call = `tools.${toolIdentifier(entry.name)} ${truncate(entry.input, 180).replaceAll("\n", " ")}`;
      result = entry.ok ? `ok, ${entry.output?.length ?? 0} chars` : `error: ${entry.error}`;
      break;
    case "agent": {
      const input = JSON.parse(entry.input);
      call = record(input)
        ? `agents.run ${input.agent} ${json(input.description ?? input.prompt)}`
        : `agents.run ${entry.input}`;
      const output = entry.output ? JSON.parse(entry.output) : undefined;
      result = entry.abandoned
        ? entry.handle
          ? `abandoned and ${entry.archived ? "archived" : `archive failed: ${entry.archive_error}`}, ${entry.handle}`
          : "abandoned (start unknown)"
        : entry.cached ? `cached from ${entry.source!.handle}`
        : entry.ok ? `${output.status}, ${entry.handle}` : `error: ${entry.error}`;
      break;
    }
    case "classify": {
      call = "models.classify";
      if (entry.ok && entry.output) {
        const answers = Object.entries(JSON.parse(entry.output).answers);
        if (answers.length === 1) call += ` ${answers[0][0]}`;
        result = answers.map(([id, answer]: [string, any]) => {
          const label = answers.length === 1 ? "" : `${id} -> `;
          if (answer.type === "choice") return `${label}${answer.choice} (p=${answer.confidence})`;
          if (answer.type === "bool") return `${label}${answer.probability}`;
          return `${label}${answer.score} (p=${answer.confidence})`;
        }).join(", ");
      } else result = `error: ${entry.error}`;
      break;
    }
  }
  return `#${entry.seq} L${entry.line} ${call} -> ${result}, ${elapsed}`.replaceAll("\n", " ");
}

export interface RunContext {
  personality?: Personality;
  host: Host;
  session: string;
  call: string;
  configDir?: string;
  judge?: JudgeConfig | null;
  judgeError?: string;
  fetchImpl?: typeof fetch;
  args?: Record<string, unknown>;
  stored?: string | null;
  saveAs?: string;
}
export interface ScriptResult {
  ok: boolean;
  output: string;
  traceFile: string;
  trace: TraceEntry[];
}
interface WorkflowMetadata {
  name: string;
  description: string;
  saved_ms: number;
  runs: number;
  last_run_ms: number;
  last_status: "completed" | "failed";
  last_error: string;
}
interface Observation extends AgentResult {
  key: string;
  state: string;
  agent: string;
  description: string;
  prompt: string;
  trace: string;
  seq: number;
  stored_ms: number;
}
const INPUT_FORMS = '{script: string, args?: object, save_as?: string}, {run: string, args?: object}, {list: true}, or {show: string}';
class WorkflowInputError extends Error {}
function workflowDirectory(context: RunContext): string {
  return resolve(context.configDir!, "workflows");
}
function storedWorkflows(context: RunContext): WorkflowMetadata[] {
  const directory = workflowDirectory(context);
  if (!existsSync(directory)) return [];
  return readdirSync(directory).filter((name) => name.endsWith(".json")).sort()
    .map((name) => JSON.parse(readFileSync(join(directory, name), "utf8")));
}
export async function executeWorkflow(input: unknown, context: RunContext): Promise<{ ok: boolean; output: string }> {
  const invalid = () => new WorkflowInputError(`workflow input must be exactly one of ${INPUT_FORMS}`);
  if (!record(input)) throw invalid();
  const keys = Object.keys(input);
  const inline = typeof input.script === "string" && keys.every((key) => ["script", "args", "save_as"].includes(key));
  const stored = typeof input.run === "string" && keys.every((key) => ["run", "args"].includes(key));
  const list = input.list === true && keys.length === 1;
  const show = typeof input.show === "string" && keys.length === 1;
  if (!inline && !stored && !list && !show) throw invalid();
  if (Object.hasOwn(input, "args") && !record(input.args)) throw invalid();
  for (const key of ["save_as", "run", "show"]) {
    if (Object.hasOwn(input, key) && (typeof input[key] !== "string" || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(input[key]))) {
      throw new WorkflowInputError(`${key} must match ^[a-z0-9][a-z0-9-]{0,63}$; forms: ${INPUT_FORMS}`);
    }
  }
  if (list) {
    const workflows = storedWorkflows(context).map(({ name, description, saved_ms, runs, last_status, last_error = "" }) => ({
      name, description, saved_ms, runs, last_status,
      ...(last_status === "failed" ? { last_error: truncate(last_error, 200) } : {}),
    }));
    return { ok: true, output: `Stored workflows:\n${json(workflows)}` };
  }
  let script = input.script;
  if (stored || show) {
    const workflows = storedWorkflows(context);
    const name = stored ? input.run : input.show;
    const metadata = workflows.find((workflow) => workflow.name === name);
    if (!metadata) throw new Error(`Unknown workflow: ${name}. Stored workflows: ${workflows.map((workflow) => workflow.name).join(", ") || "(none)"}`);
    script = readFileSync(join(workflowDirectory(context), `${name}.js`), "utf8");
    if (show) return { ok: true, output: json({ script, metadata }) };
  }
  return executeScript(script, { ...context, args: input.args ?? {}, stored: stored ? input.run : null, saveAs: input.save_as });
}
export async function executeScript(script: string, context: RunContext): Promise<ScriptResult> {
  const personality = context.personality ?? PERSONALITIES.workflow;
  const started = Date.now();
  const component = (value: string) => encodeURIComponent(value).replaceAll(".", "%2E");
  const traceDir = resolve(context.configDir ?? tmpdir(), "traces", component(context.session));
  const traceFile = join(traceDir, `${component(context.call)}.jsonl`);
  let options = { ...DEFAULT_OPTIONS };
  const items: string[] = [];
  const logs: string[] = [];
  const trace: TraceEntry[] = [];
  // Summaries use the complete values; trace record fields have their own 4000-char budget.
  const lines: string[] = [];
  const controller = new AbortController();
  const agents = new Map<TraceEntry, { started: Promise<void>; controller: AbortController; input: string }>();
  let active = true;
  let failure: string | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let worker: Worker | undefined;
  const ensureActive = () => {
    if (!active) throw new Error("Script has ended; outstanding calls were abandoned");
    controller.signal.throwIfAborted();
  };
  const tracked = (kind: TraceEntry["kind"], name: string, run: (input: any, entry: TraceEntry) => Promise<any>) => (input: any, line: number) => {
    ensureActive();
    // Snapshot input before crossing the host boundary or awaiting another call.
    const rawInput = json(input);
    const entry: TraceEntry = { seq: trace.length + 1, line, kind, name, input: truncate(rawInput, 4000), ok: false, started_ms: Date.now(), ms: 0, ...(kind === "agent" ? { result: null, state_before: null, state_after: null } : {}) };
    trace.push(entry);
    const index = entry.seq - 1;
    const pending = (async () => {
      try {
        const output = await run(JSON.parse(rawInput), entry);
        ensureActive();
        entry.ok = true;
        const rawOutput = json(output);
        entry.output = truncate(rawOutput, 4000);
        if (kind === "agent") {
          entry.handle = output.handle;
          entry.session = output.session;
          entry.result = output.result;
        }
        entry.ms = Date.now() - entry.started_ms;
        // Render before cutting JSON; large outputs need not be parseable after truncation.
        lines[index] = traceLine({ ...entry, input: rawInput, output: rawOutput });
        return output;
      } catch (error) {
        if (active) {
          entry.error = message(error);
          entry.ms = Date.now() - entry.started_ms;
          lines[index] = traceLine({ ...entry, input: rawInput });
        }
        throw error;
      }
    })();
    // Unawaited failures must not crash the plugin after the script has returned.
    void pending.catch(() => {});
    return pending;
  };
  try {
    options = parseOptions(script);
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`Script timed out after ${options.timeout_ms} ms`)), options.timeout_ms);
    });
    const run = async () => {
      const schemas: ToolSchema[] = personality.tools ? (await context.host("tools.list", {}, controller.signal)).tools : [];
      ensureActive();
      const problems = checkScript(script, schemas, personality);
      if (problems.length) throw new Error(renderDiagnostics(script, problems));
      await new Promise<void>((resolveRun, rejectRun) => {
        // A worker lets the parent enforce the deadline even for a CPU loop
        // resumed after an await, outside vm.Script's synchronous timeout.
        worker = new Worker(new URL(import.meta.url), { workerData: { script, args: context.args ?? {}, options, personality, schemas } });
        worker.on("error", rejectRun);
        worker.on("exit", (code) => { if (active) rejectRun(new Error(`Script worker exited with code ${code}`)); });
        worker.on("message", (event) => {
          if (!active) return;
          switch (event.type) {
            case "text": items.push(event.value); return;
            case "log": logs.push(event.value); return;
            case "done": resolveRun(); return;
            case "failed": rejectRun(new Error(event.error)); return;
            case "call": {
              const invoke = tracked(event.kind, event.name, async (input, entry) => {
                switch (event.kind) {
                  case "tool":
                    if (!record(input)) throw new Error(`tools.${event.name} expects an argument object`);
                    return (await context.host("tool.call", { name: event.name, input }, controller.signal)).output;
                  case "agent": {
                    const prepared = prepareAgent(input, personality);
                    const start = Promise.withResolvers<void>();
                    const startController = new AbortController();
                    agents.set(entry, { started: start.promise, controller: startController, input: json(input) });
                    try {
                      const before = (await context.host("workspace.state", {}, controller.signal)).state as string | null;
                      ensureActive();
                      entry.state_before = before;
                      const key = createHash("sha256").update(json([before, input.agent, prepared.task.prompt, input.model ?? null, input.effort ?? null])).digest("hex");
                      const memoDir = resolve(context.configDir ?? tmpdir(), "memo");
                      const memoFile = join(memoDir, `${key}.json`);
                      // A command can depend on, or change, what the state does not cover; a scout only reads files.
                      const remember = input.agent === "orchestrator-scout" && input.cache !== false && before !== null;
                      if (remember && existsSync(memoFile)) {
                        const memo: Observation = JSON.parse(readFileSync(memoFile, "utf8"));
                        if (memo.key === key && memo.state === before && memo.status === "done") {
                          entry.cached = true;
                          entry.source = { handle: memo.handle, session: memo.session, trace: memo.trace, seq: memo.seq };
                          return { handle: memo.handle, session: memo.session, status: memo.status, report: memo.report, result: memo.result, cached: true };
                        }
                      }
                      const result = await runAgent(input, context.host, controller.signal, (result) => {
                        entry.handle = result.handle;
                        entry.session = result.session;
                        entry.result = result.result;
                        start.resolve();
                      }, startController.signal, personality);
                      ensureActive();
                      const after = (await context.host("workspace.state", {}, controller.signal)).state as string | null;
                      ensureActive();
                      entry.state_after = after;
                      if (remember && after === before && result.status === "done") {
                        const memo: Observation = { key, state: before, agent: input.agent, description: prepared.task.description, prompt: prepared.task.prompt, ...result, trace: traceFile, seq: entry.seq, stored_ms: Date.now() };
                        mkdirSync(memoDir, { recursive: true });
                        writeFileSync(memoFile, json(memo) + "\n");
                      }
                      return result;
                    } finally {
                      start.resolve();
                    }
                  }
                  case "classify":
                    if (context.judgeError) throw new Error(context.judgeError);
                    return classify(context.judge ?? null, input, controller.signal, context.fetchImpl);
                }
              });
              void invoke(event.input, event.line).then(
                (output) => { if (active) worker!.postMessage({ id: event.id, output }); },
                (error) => { if (active) worker!.postMessage({ id: event.id, error: message(error) }); },
              );
            }
          }
        });
      });
    };
    await Promise.race([run(), deadline]);
  } catch (error) {
    failure = message(error);
  } finally {
    active = false;
    clearTimeout(timer);
    controller.abort(new Error(failure ?? "Script completed; outstanding calls abandoned"));
    await worker?.terminate();
  }
  const abandonedAgents = [...agents].filter(([entry]) => entry.output === undefined && entry.error === undefined);
  if (abandonedAgents.length) {
    let grace: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        Promise.all(abandonedAgents.map(([, state]) => state.started)),
        new Promise<void>((resolve) => { grace = setTimeout(resolve, 5000); }),
      ]);
    } finally {
      clearTimeout(grace);
      for (const [, state] of abandonedAgents) state.controller.abort();
    }
    await Promise.all(abandonedAgents.map(async ([entry, state]) => {
      entry.abandoned = true;
      if (entry.handle) {
        try {
          await callHostTool(context.host, "archive", { target: entry.handle, reason: "Workflow script ended while agents.run was pending" }, new AbortController().signal);
          entry.archived = true;
        } catch (error) {
          entry.archived = false;
          entry.archive_error = message(error);
        }
      }
      entry.error = "Abandoned when the script ended";
      entry.ms = Date.now() - entry.started_ms;
      lines[entry.seq - 1] = traceLine({ ...entry, input: state.input });
    }));
  }
  for (const entry of trace) {
    if (entry.output === undefined && entry.error === undefined) {
      entry.error = "Abandoned when the script ended";
      entry.ms = Date.now() - entry.started_ms;
      // Inputs may be cut JSON; abandoned lines need no parsing.
      lines[entry.seq - 1] = `#${entry.seq} L${entry.line} ${entry.kind === "tool" ? `tools.${toolIdentifier(entry.name)}` : entry.name} -> abandoned, ${(entry.ms / 1000).toFixed(1)} s`;
    }
  }
  const status = failure === undefined ? "completed" : "failed";
  if (context.stored) {
    const path = join(workflowDirectory(context), `${context.stored}.json`);
    const metadata: WorkflowMetadata = JSON.parse(readFileSync(path, "utf8"));
    writeFileSync(path, json({ ...metadata, runs: metadata.runs + 1, last_run_ms: Date.now(), last_status: status, last_error: failure ?? "" }) + "\n");
  }
  if (context.saveAs) {
    const directory = workflowDirectory(context);
    mkdirSync(directory, { recursive: true });
    const now = Date.now();
    const firstLine = script.split("\n")[0];
    const description = /^\s*\/\//.test(firstLine) && !/^\s*\/\/\s*@options\s*:/.test(firstLine)
      ? firstLine.replace(/^\s*\/\/\s?/, "").trim() : "";
    writeFileSync(join(directory, `${context.saveAs}.js`), script);
    const metadata: WorkflowMetadata = { name: context.saveAs, description, saved_ms: now, runs: 1, last_run_ms: now, last_status: status, last_error: failure ?? "" };
    writeFileSync(join(directory, `${context.saveAs}.json`), json(metadata) + "\n");
  }
  const header = `${personality.label}${context.stored ? ` ${context.stored}` : ""} ${status} in ${((Date.now() - started) / 1000).toFixed(1)} s`;
  const savedLine = context.saveAs ? `\nsaved as ${context.saveAs}` : "";
  const fullBody = [...items, ...(logs.length ? [`<console_output>\n${logs.join("\n")}\n</console_output>`] : [])].join("\n");
  const errorLine = failure === undefined ? "" : `\nScript error: ${failure}`;
  const reused = trace.filter((entry) => entry.cached).length;
  const reusedLine = reused ? `\nreused observations: ${reused}` : "";
  const footer = `\n</trace>${reusedLine}\ntrace file: ${traceFile}`;
  const fullOutput = `${header}${savedLine}\n${fullBody}${errorLine}\n<trace>\n${lines.join("\n")}${footer}`;
  mkdirSync(traceDir, { recursive: true });
  writeFileSync(traceFile, [
    { script, args: context.args ?? {}, stored: context.stored ?? context.saveAs ?? null, options },
    ...trace,
    { status, output: fullOutput },
  ].map((value) => JSON.stringify(value)).join("\n") + "\n");

  let prefix = `${header}${savedLine}\n${truncate(fullBody, options.max_output_chars)}${truncate(errorLine, 1600)}\n<trace>\n`;
  // Reserve trace space even if an unusually long config directory consumes the budget.
  prefix = truncate(prefix, Math.max(0, MAX_RESULT_CHARS - footer.length - 400));
  const available = Math.max(0, MAX_RESULT_CHARS - prefix.length - footer.length);
  let renderedTrace = lines.join("\n");
  if (renderedTrace.length > available) {
    const omitted = lines.length > 2 ? `\n… ${lines.length - 2} trace lines omitted …\n` : "\n…\n";
    if (lines.length > 1) {
      const half = Math.max(0, Math.floor((available - omitted.length) / 2));
      renderedTrace = truncate(lines[0], half) + omitted + truncate(lines.at(-1)!, half);
    } else renderedTrace = truncate(renderedTrace, available);
  }
  return { ok: failure === undefined, output: `${prefix}${renderedTrace}${footer}`, traceFile, trace };
}

async function runVMWorker(): Promise<void> {
  const { script, args, options, personality, schemas }: { script: string; args: Record<string, unknown>; options: Options; personality: Personality; schemas: ToolSchema[] } = workerData;
  const port = parentPort!;
  const pending = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
  let sequence = 0;
  port.on("message", (event) => {
    const call = pending.get(event.id)!;
    pending.delete(event.id);
    if (event.error !== undefined) call.reject(new Error(event.error));
    else call.resolve(event.output);
  });
  const invoke = (kind: TraceEntry["kind"], name: string) => (input: unknown) => {
    const id = ++sequence;
    const line = scriptLine();
    const snapshot = JSON.parse(json(input));
    const result = new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      port.postMessage({ type: "call", id, kind, name, line, input: snapshot });
    });
    void result.catch(() => {});
    return result;
  };
  const globals = {
    get tools(): never { throw new Error("A workflow can only call agents.run and models.classify"); },
    args,
    agents: { run: invoke("agent", "agents.run") },
    models: { classify: invoke("classify", "models.classify") },
    text: (value: unknown) => port.postMessage({ type: "text", value: typeof value === "string" ? value : json(value) }),
    console: { log: (...values: unknown[]) => port.postMessage({ type: "log", value: values.map((value) => typeof value === "string" ? value : json(value)).join(" ") }) },
  };
  if (personality.tools) {
    const tools = Object.create(null);
    for (const schema of schemas) {
      const name = toolIdentifier(schema.name);
      if (Object.hasOwn(tools, name)) throw new Error(`tool identifier collision: ${name}`);
      tools[name] = invoke("tool", schema.name);
    }
    Object.defineProperty(globals, "tools", { value: tools });
    Object.assign(globals, {
      ALL_TOOLS: schemas.map(({ name, description }) => ({ name, description })),
      describeTool: (name: string) => schemas.find((schema) => schema.name === name),
    });
  }
  const sandbox = vm.createContext(globals);
  const compiled = new vm.Script(wrapScript(script), { filename: personality.file });
  const value = await compiled.runInContext(sandbox, { timeout: options.timeout_ms });
  if (value !== undefined) port.postMessage({ type: "text", value: typeof value === "string" ? value : json(value) });
  port.postMessage({ type: "done" });
}

export const INITIALIZE_RESULT = {
  protocol_version: 1,
  plugin: { id: "orchestrator", version: "1.0.0", kind: "bun" },
  hooks: [],
  skills: [],
  tools: [{
    name: "workflow",
    description: "Orchestrate specialised workers and an optional judge with a traced JavaScript async-function body. Use {script, args?, save_as?}, {run, args?}, {list:true}, or {show:name}. Globals: args, agents.run (agent required: orchestrator-scout, orchestrator-runner, orchestrator-editor; a scout's answer on an unchanged workspace is remembered unless cache:false), models.classify, text, console.log. No local tools, imports, process, fetch, Bun, or timers. A first-line // @options: JSON comment sets timeout_ms (default 1800000, max 3000000) and max_output_chars (default 3000, max 3600). Scripts are saved even on failure for inspection and repair with show. Before anything runs, the script is checked for syntax errors and unavailable globals; one that fails the check makes no call, and the result names the line and column to fix.",
    inputSchema: {
      type: "object",
      oneOf: [
        { type: "object", properties: { script: { type: "string" }, args: { type: "object" }, save_as: { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,63}$" } }, required: ["script"], additionalProperties: false },
        { type: "object", properties: { run: { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,63}$" }, args: { type: "object" } }, required: ["run"], additionalProperties: false },
        { type: "object", properties: { list: { const: true } }, required: ["list"], additionalProperties: false },
        { type: "object", properties: { show: { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,63}$" } }, required: ["show"], additionalProperties: false },
      ],
    },
  }],
};

export function initializeResult(personality: Personality) {
  return { ...INITIALIZE_RESULT, plugin: { ...INITIALIZE_RESULT.plugin, id: personality.plugin },
    tools: [{ ...INITIALIZE_RESULT.tools[0], name: personality.name, description: personality.tools ? "Run a JavaScript program that calls your other tools and can start worker agents. Only what the program outputs comes back to you, so one call can run several tools, run them in parallel, branch on their results, and filter or combine them before you read them.\n\nInput is one of: `{script, args?, save_as?}` runs a program (`save_as` stores it under a name, also when it fails); `{run, args?}` runs a stored program with new `args`; `{list: true}` lists stored programs; `{show: name}` returns a stored program's source and last error so you can repair it.\n\n`script` is the body of an async function: top-level `await` and `return` work. It has no file, network, import, or timer access; it reaches the outside only through these globals:\n\n- `args`: the `args` object of the call, or `{}`.\n- `await tools.<name>(args)` calls a tool with the arguments you would pass it directly and resolves to the tool's result value. A character of the tool name that cannot be part of a JavaScript identifier becomes `_`. File and shell tools resolve to an object such as `{title, output, metadata}` whose `output` is the text you would see when calling the tool directly. A failed or denied call rejects with an `Error` carrying the tool's error text; use `Promise.allSettled` to keep the results of the calls that succeed.\n- `ALL_TOOLS` is `[{name, description}]` for every tool the program can call. `describeTool(name)` returns `{name, description, input_schema}` or `undefined`.\n- `await agents.run({agent, prompt, description?, result?, model?, effort?, timeout_secs?, cache?})` starts one worker agent you may spawn and waits for its report; it resolves to `{handle, session, status, report, result}`. `result` maps field names to descriptions: the worker must end its report with a `RESULT: {...}` line holding those fields, which comes back parsed as `result` (or `null`). A worker gets 600 seconds unless you pass `timeout_secs` (at most 1800); one that runs out is stopped. Start several with `Promise.all`.\n- `await models.classify({state, questions})` asks the configured judge model typed questions (`bool`, `choice`, `score`) about `state`; it rejects when no judge is configured.\n- `text(value)` adds an item to the output (a string as is, anything else as JSON); `return value` does the same. `console.log(...)` adds a line to a separate console block.\n- An optional first line sets limits: `// @options: {\"timeout_ms\": 1800000, \"max_output_chars\": 3000}` (at most 3000000 and 3600).\n\nOutput longer than `max_output_chars` keeps only its start and end, so select what you need inside the program: slice file text, filter matches, summarise command output. The result ends with one trace line per tool, agent, or judge call (source line, call, arguments, outcome, time). Calls are real: calls made before an error are not undone. Before anything runs, the program is checked for syntax errors, unknown tools, missing required arguments, and unavailable globals; a program that fails the check makes no call, and the result names the line and column to fix." : INITIALIZE_RESULT.tools[0].description }] };
}

interface Frame {
  id?: number | string | null;
  method?: string;
  params?: any;
  result?: any;
  error?: { code: number; message: string };
}
export class CapabilityClient {
  private sequence = 0;
  private pending = new Map<number | string, { resolve: (value: any) => void; reject: (error: Error) => void; dispose: () => void }>();
  constructor(private write: (frame: Record<string, unknown>) => void) {}
  call(capability: string, session: string, call: string, method: string, params: Record<string, unknown>, signal: AbortSignal): Promise<any> {
    signal.throwIfAborted();
    const id = ++this.sequence;
    return new Promise((resolve, reject) => {
      const abort = () => { this.pending.delete(id); reject(signal.reason); };
      signal.addEventListener("abort", abort, { once: true });
      this.pending.set(id, { resolve, reject, dispose: () => signal.removeEventListener("abort", abort) });
      this.write({ jsonrpc: "2.0", id, method: "host/capability", params: { capability, session, call, method, params } });
    });
  }
  accept(frame: Frame): boolean {
    if (frame.method !== undefined) return false;
    const pending = frame.id === undefined || frame.id === null ? undefined : this.pending.get(frame.id);
    if (pending) {
      this.pending.delete(frame.id!);
      pending.dispose();
      if (frame.error) pending.reject(new Error(frame.error.message));
      else pending.resolve(frame.result);
    }
    return true;
  }
}

async function main(): Promise<void> {
  const name = process.argv[2];
  if (name !== "workflow" && name !== "run") throw new Error("Expected personality: workflow or run");
  const personality = PERSONALITIES[name];
  let judge: JudgeConfig | null = null;
  let judgeError: string | undefined;
  try {
    if (process.env.HYA_BUNDLE_CONFIG_FILE) judge = parseJudgeConfig(Bun.YAML.parse(readFileSync(process.env.HYA_BUNDLE_CONFIG_FILE, "utf8")));
  } catch (error) {
    judgeError = `Invalid judge config: ${message(error)}`;
    process.stderr.write(`orchestrator: ${judgeError}\n`);
  }
  const write = (frame: Record<string, unknown>) => { process.stdout.write(JSON.stringify(frame) + "\n"); };
  const cap = new CapabilityClient(write);
  const active = new Set<Promise<void>>();
  let closing = false;
  const handle = async (frame: Frame) => {
    const respond = (result: unknown) => write({ jsonrpc: "2.0", id: frame.id, result });
    const error = (code: number, message: string) => write({ jsonrpc: "2.0", id: frame.id, error: { code, message } });
    if (frame.id === undefined || frame.id === null) return;
    switch (frame.method) {
      case "initialize": respond(initializeResult(personality)); return;
      case "shutdown":
        closing = true;
        await Promise.all(active);
        process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result: {} }) + "\n", () => process.exit(0));
        return;
      case "tool/call": {
        const p = frame.params;
        if (!record(p) || p.tool !== personality.name || typeof p.session !== "string" || typeof p.call !== "string" || typeof p.host_capability !== "string") {
          error(-32602, `${personality.name} requires ${INPUT_FORMS}, session, call, and a host_capability`);
          return;
        }
        const start = Date.now();
        try {
          const result = await executeWorkflow(p.input, {
            session: p.session, call: p.call, configDir: process.env.HYA_BUNDLE_CONFIG_DIR, judge, judgeError, personality,
            host: (method, params, signal) => cap.call(p.host_capability, p.session, p.call, method, params, signal),
          });
          respond({ ok: result.ok, output: result.output, time_ms: Date.now() - start });
        } catch (failure) {
          error(failure instanceof WorkflowInputError ? -32602 : -32003, message(failure));
        }
        return;
      }
      default: error(-32601, `Unknown method: ${frame.method}`);
    }
  };
  const reader = createInterface({ input: process.stdin, terminal: false });
  for await (const line of reader) {
    if (!line.trim()) continue;
    let frame: Frame;
    try { frame = JSON.parse(line); }
    catch { process.stderr.write("orchestrator: ignoring a non-JSON line\n"); continue; }
    if (!record(frame)) continue;
    if (cap.accept(frame)) continue;
    if (closing) continue;
    const pending = handle(frame).catch((error) => {
      process.stderr.write(`orchestrator: ${message(error)}\n`);
      if (frame.id !== undefined && frame.id !== null) write({ jsonrpc: "2.0", id: frame.id, error: { code: -32603, message: message(error) } });
    });
    if (frame.method !== "shutdown") {
      active.add(pending);
      void pending.finally(() => active.delete(pending));
    }
  }
  await Promise.all(active);
}
if (!isMainThread) void runVMWorker().catch((error) => parentPort!.postMessage({ type: "failed", error: located(error) }));
else if (import.meta.main) void main().catch((error) => { process.stderr.write(`orchestrator: ${message(error)}\n`); process.exit(1); });
