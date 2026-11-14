// Self-contained hya plugin protocol v1 process; stdout is NDJSON only.
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { createInterface } from "node:readline";
import vm from "node:vm";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

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
}
export interface AgentResult {
  handle: string;
  session: string;
  status: "done" | "failed" | "cancelled" | "timeout";
  report: string;
  result: Record<string, unknown> | null;
}
export interface TraceEntry {
  seq: number;
  line: number;
  kind: "agent" | "classify";
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
  abandoned?: boolean;
  archived?: boolean;
  archive_error?: string;
}
const SCRIPT_FILE = "orchestrator-script.js";
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

export async function runAgent(input: AgentInput, host: Host, signal: AbortSignal, onStarted?: (result: AgentResult) => void, startSignal = signal): Promise<AgentResult> {
  if (!record(input)) throw new Error("agents.run requires an argument object");
  string(input.prompt, "agents.run prompt");
  const agent = string(input.agent, "agents.run agent");
  if (!WORKER_AGENTS.includes(agent)) throw new Error(`agents.run agent must be one of: ${WORKER_AGENTS.join(", ")}`);
  let prompt = input.prompt;
  if (input.result !== undefined) {
    if (!record(input.result)) throw new Error("agents.run result must map field names to descriptions");
    for (const description of Object.values(input.result)) string(description, "agents.run result description");
    prompt += "\n\nEnd your report with one final line of the form RESULT: {...} — a single-line JSON object with exactly these fields:"
      + Object.entries(input.result).map(([field, description]) => `\n- ${field}: ${description}`).join("");
  }
  const timeout = input.timeout_secs === undefined ? 1800 : input.timeout_secs;
  if (typeof timeout !== "number" || !Number.isFinite(timeout) || timeout < 0 || timeout > 1800) {
    throw new Error("agents.run timeout_secs must be in 0..=1800");
  }
  const call = (name: string, args: Record<string, unknown>) => callHostTool(host, name, args, signal);
  const task = await callHostTool(host, "task", { tasks: [{
    subagent_type: agent,
    prompt,
    description: input.description ?? input.prompt.slice(0, 80),
    ...(input.model === undefined ? {} : { model: input.model }),
    ...(input.effort === undefined ? {} : { effort: input.effort }),
  }] }, startSignal);
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
  const deadline = Date.now() + timeout * 1000;
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
    if (metadata.woke_by === "timeout" || Date.now() >= deadline) return result;
    // Mail/stall wakes are not terminal; wait again within the same deadline.
  }
}

function scriptLine(): number {
  const match = new Error().stack?.match(/orchestrator-script\.js:(\d+):\d+/);
  if (!match) throw new Error("could not locate the script call site");
  return Number(match[1]);
}
function traceLine(entry: TraceEntry): string {
  const elapsed = `${(entry.ms / 1000).toFixed(1)} s`;
  let call: string;
  let result: string;
  switch (entry.kind) {
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
}
const INPUT_FORMS = '{script: string, args?: object, save_as?: string}, {run: string, args?: object}, or {list: true}';
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
  if (!inline && !stored && !list) throw invalid();
  if (Object.hasOwn(input, "args") && !record(input.args)) throw invalid();
  for (const key of ["save_as", "run"]) {
    if (Object.hasOwn(input, key) && (typeof input[key] !== "string" || !/^[a-z0-9][a-z0-9-]{0,63}$/.test(input[key]))) {
      throw new WorkflowInputError(`${key} must match ^[a-z0-9][a-z0-9-]{0,63}$; forms: ${INPUT_FORMS}`);
    }
  }
  if (list) {
    return { ok: true, output: `Stored workflows:\n${json(storedWorkflows(context).map(({ name, description, saved_ms, runs }) => ({ name, description, saved_ms, runs })))}` };
  }
  let script = input.script;
  if (stored) {
    const names = storedWorkflows(context).map((workflow) => workflow.name);
    if (!names.includes(input.run)) throw new Error(`Unknown workflow: ${input.run}. Stored workflows: ${names.join(", ") || "(none)"}`);
    script = readFileSync(join(workflowDirectory(context), `${input.run}.js`), "utf8");
  }
  return executeScript(script, { ...context, args: input.args ?? {}, stored: stored ? input.run : null, saveAs: input.save_as });
}
export async function executeScript(script: string, context: RunContext): Promise<ScriptResult> {
  const started = Date.now();
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
    const entry: TraceEntry = { seq: trace.length + 1, line, kind, name, input: truncate(rawInput, 4000), ok: false, started_ms: Date.now(), ms: 0, ...(kind === "agent" ? { result: null } : {}) };
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
      await new Promise<void>((resolveRun, rejectRun) => {
        // A worker lets the parent enforce the deadline even for a CPU loop
        // resumed after an await, outside vm.Script's synchronous timeout.
        worker = new Worker(new URL(import.meta.url), { workerData: { script, args: context.args ?? {}, options } });
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
                  case "agent": {
                    const start = Promise.withResolvers<void>();
                    const startController = new AbortController();
                    agents.set(entry, { started: start.promise, controller: startController, input: json(input) });
                    return runAgent(input, context.host, controller.signal, (result) => {
                      entry.handle = result.handle;
                      entry.session = result.session;
                      entry.result = result.result;
                      start.resolve();
                    }, startController.signal).finally(() => start.resolve());
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
      lines[entry.seq - 1] = `#${entry.seq} L${entry.line} ${entry.name} -> abandoned, ${(entry.ms / 1000).toFixed(1)} s`;
    }
  }
  const status = failure === undefined ? "completed" : "failed";
  if (context.stored) {
    const path = join(workflowDirectory(context), `${context.stored}.json`);
    const metadata: WorkflowMetadata = JSON.parse(readFileSync(path, "utf8"));
    writeFileSync(path, json({ ...metadata, runs: metadata.runs + 1, last_run_ms: Date.now(), last_status: status }) + "\n");
  }
  if (context.saveAs && failure === undefined) {
    const directory = workflowDirectory(context);
    mkdirSync(directory, { recursive: true });
    const now = Date.now();
    const firstLine = script.split("\n")[0];
    const description = /^\s*\/\//.test(firstLine) && !/^\s*\/\/\s*@options\s*:/.test(firstLine)
      ? firstLine.replace(/^\s*\/\/\s?/, "").trim() : "";
    writeFileSync(join(directory, `${context.saveAs}.js`), script);
    const metadata: WorkflowMetadata = { name: context.saveAs, description, saved_ms: now, runs: 1, last_run_ms: now, last_status: status };
    writeFileSync(join(directory, `${context.saveAs}.json`), json(metadata) + "\n");
  }
  const header = `Workflow${context.stored ? ` ${context.stored}` : ""} ${status} in ${((Date.now() - started) / 1000).toFixed(1)} s`;
  const savedLine = context.saveAs && failure === undefined ? `\nsaved as ${context.saveAs}` : "";
  const fullBody = [...items, ...(logs.length ? [`<console_output>\n${logs.join("\n")}\n</console_output>`] : [])].join("\n");
  const errorLine = failure === undefined ? "" : `\nScript error: ${failure}`;
  const component = (value: string) => encodeURIComponent(value).replaceAll(".", "%2E");
  const traceDir = resolve(context.configDir ?? tmpdir(), "traces", component(context.session));
  const traceFile = join(traceDir, `${component(context.call)}.jsonl`);
  const fullOutput = `${header}${savedLine}\n${fullBody}${errorLine}\n<trace>\n${lines.join("\n")}\n</trace>\ntrace file: ${traceFile}`;
  mkdirSync(traceDir, { recursive: true });
  writeFileSync(traceFile, [
    { script, args: context.args ?? {}, stored: context.stored ?? (failure === undefined ? context.saveAs ?? null : null), options },
    ...trace,
    { status, output: fullOutput },
  ].map((value) => JSON.stringify(value)).join("\n") + "\n");

  const footer = `\n</trace>\ntrace file: ${traceFile}`;
  let prefix = `${header}${savedLine}\n${truncate(fullBody, options.max_output_chars)}${truncate(errorLine, 400)}\n<trace>\n`;
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
  const { script, args, options }: { script: string; args: Record<string, unknown>; options: Options } = workerData;
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
  const sandbox = vm.createContext(globals);
  const compiled = new vm.Script(`(async function() {${script}\n})()`, { filename: SCRIPT_FILE });
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
    description: "Orchestrate specialised workers and an optional judge with a traced JavaScript async-function body. Use {script, args?, save_as?}, {run, args?}, or {list:true}. Globals: args, agents.run (agent required: orchestrator-scout, orchestrator-runner, orchestrator-editor), models.classify, text, console.log. No local tools, imports, process, fetch, Bun, or timers. A first-line // @options: JSON comment sets timeout_ms (default 1800000, max 3000000) and max_output_chars (default 3000, max 3600). Completed scripts can be saved and reused with new args.",
    inputSchema: {
      type: "object",
      oneOf: [
        { type: "object", properties: { script: { type: "string" }, args: { type: "object" }, save_as: { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,63}$" } }, required: ["script"], additionalProperties: false },
        { type: "object", properties: { run: { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,63}$" }, args: { type: "object" } }, required: ["run"], additionalProperties: false },
        { type: "object", properties: { list: { const: true } }, required: ["list"], additionalProperties: false },
      ],
    },
  }],
};

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
      case "initialize": respond(INITIALIZE_RESULT); return;
      case "shutdown":
        closing = true;
        await Promise.all(active);
        process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: frame.id, result: {} }) + "\n", () => process.exit(0));
        return;
      case "tool/call": {
        const p = frame.params;
        if (!record(p) || p.tool !== "workflow" || typeof p.session !== "string" || typeof p.call !== "string" || typeof p.host_capability !== "string") {
          error(-32602, `workflow requires ${INPUT_FORMS}, session, call, and a host_capability`);
          return;
        }
        const start = Date.now();
        try {
          const result = await executeWorkflow(p.input, {
            session: p.session, call: p.call, configDir: process.env.HYA_BUNDLE_CONFIG_DIR, judge, judgeError,
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
if (!isMainThread) void runVMWorker().catch((error) => parentPort!.postMessage({ type: "failed", error: message(error) }));
else if (import.meta.main) void main().catch((error) => { process.stderr.write(`orchestrator: ${message(error)}\n`); process.exit(1); });
