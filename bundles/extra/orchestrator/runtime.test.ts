import { afterAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import {
  CapabilityClient, DEFAULT_OPTIONS, INITIALIZE_RESULT, buildJevRequest, classify,
  callHostTool, executeScript, executeWorkflow, parseJudgeConfig, parseOptions, runAgent as runAgentRaw, truncate,
  type AgentInput, type AgentResult, type ClassifyInput, type Host, type RunContext,
} from "./runtime.ts";

const directory = mkdtempSync(join(import.meta.dir, ".runtime-test-"));
const received: Array<{ method: string; params: any }> = [];
const checkedHost = (host: Host): Host => async (method, params, signal) => {
  received.push({ method, params });
  expect(method).toBe("tool.call");
  expect(["task", "wait", "archive", "read"]).toContain(params.name);
  if (params.name === "read") expect(params.input.path).toStartWith("channel://");
  return host(method, params, signal);
};
afterAll(() => {
  for (const { method, params } of received) {
    expect(method).toBe("tool.call");
    expect(["task", "wait", "archive", "read"]).toContain(params.name);
    if (params.name === "read") expect(params.input.path).toStartWith("channel://");
  }
  rmSync(directory, { recursive: true, force: true });
});
let nextCall = 0;
const host: Host = async () => ({ output: { metadata: { members: [{ member: "main/worker", session: "child", status: "done", summary: "ok" }] } } });
const context = (overrides: Partial<RunContext> = {}): RunContext => ({
  session: "test-session", call: `call-${++nextCall}`, configDir: directory, ...overrides, host: checkedHost(overrides.host ?? host),
});
const run = (script: string, overrides: Partial<RunContext> = {}) => executeScript(script, context(overrides));
const workflow = (input: unknown, overrides: Partial<RunContext> = {}) => executeWorkflow(input, context(overrides));
const runAgent = (input: AgentInput, host: Host, signal: AbortSignal, onStarted?: (result: AgentResult) => void) => runAgentRaw(input, checkedHost(host), signal, onStarted);
const records = (path: string) => readFileSync(path, "utf8").trim().split("\n").map((line) => JSON.parse(line));
const judge = parseJudgeConfig({ judge: { api_key: "test-key" } })!;
const questions: ClassifyInput["questions"] = {
  ready: { type: "bool", instructions: "Is it ready?", criteria: { true: "Works", false: "Broken" } },
  kind: { type: "choice", instructions: "Choose", criteria: { repair: "Broken", finish: "Works" } },
  quality: { type: "score", instructions: "Score", criteria: ["Bad", "Good", "Great"] },
};
const jevAnswers = {
  ready: { type: "noul", noul: 0.9 },
  kind: { type: "choice", choice: "repair", probabilities: { repair: 0.97, finish: 0.03 }, confidence: 0.97 },
  quality: { type: "score", score: 1.8, confidence: 0.8 },
};
const fakeFetch = (async () => Response.json({ answers: jevAnswers })) as typeof fetch;

describe("script VM and trace", () => {
  test("concurrent calls preserve start order, script lines, full sizes, and filtered output", async () => {
    const first = Promise.withResolvers<any>();
    const second = Promise.withResolvers<any>();
    const bothStarted = Promise.withResolvers<void>();
    const calls: string[] = [];
    const pending = run([
      'text("started");',
      'const count = 2;',
      'const a = agents.run({agent:"orchestrator-scout",prompt:"first"});',
      'const b = agents.run({agent:"orchestrator-scout",prompt:"second"});',
      'const values = await Promise.all([a, b]);',
      'console.log("kept", values.length);',
      'return values.filter(v => v.report.startsWith("kept")).map(v => v.report);',
    ].join("\n"), { host: async (_method, params) => {
      calls.push(params.input.tasks[0].prompt);
      if (calls.length === 2) bothStarted.resolve();
      return await (calls.length === 1 ? first.promise : second.promise);
    } });
    await bothStarted.promise;
    second.resolve({ output: { metadata: { members: [{ member: "main/second", session: "second", status: "done", summary: "discard" }] } } });
    first.resolve({ output: { metadata: { members: [{ member: "main/first", session: "first", status: "done", summary: "kept".repeat(1500) }] } } });
    const result = await pending;
    expect(result.ok).toBe(true);
    expect(calls).toEqual(["first", "second"]);
    expect(result.trace.map(({ seq, line, name }) => ({ seq, line, name }))).toEqual([
      { seq: 1, line: 3, name: "agents.run" }, { seq: 2, line: 4, name: "agents.run" },
    ]);
    expect(result.output).toContain('<console_output>\nkept 2\n</console_output>');
    expect(result.output.indexOf("#1")).toBeLessThan(result.output.indexOf("#2"));
    expect(result.trace[0].output!.length).toBe(4000);
    const log = records(result.traceFile);
    expect(log[0]).toMatchObject({ script: expect.stringContaining("const a = agents.run"), args: {}, stored: null, options: DEFAULT_OPTIONS });
    expect(log.at(-1).status).toBe("completed");
    expect(log.at(-1).output).toContain("kept".repeat(1500));
    expect(log.at(-1).output.split("<trace>")[0]).not.toContain("discard");
  });

  test("only specified effects and standard JavaScript globals are available", async () => {
    const result = await run(`return {
      require: typeof require, process: typeof process, fetch: typeof fetch,
      Bun: typeof Bun, setTimeout: typeof setTimeout, setInterval: typeof setInterval,
      consoleWarn: typeof console.warn, Promise: typeof Promise, JSON: typeof JSON,
      ALL_TOOLS: typeof ALL_TOOLS, describeTool: typeof describeTool, args
    };`);
    expect(result.ok).toBe(true);
    for (const key of ["require", "process", "fetch", "Bun", "setTimeout", "setInterval", "consoleWarn", "ALL_TOOLS", "describeTool"]) {
      expect(result.output).toContain(`"${key}":"undefined"`);
    }
    expect(result.output).toContain('"Promise":"function"');
    expect(result.output).toContain('"JSON":"object"');
    expect(result.output).toContain('"args":{}');
  });

  test("removed tools reject with workflow guidance and never reach the host", async () => {
    for (const expression of ['tools', 'typeof tools', 'tools.bash({command:"pwd"})', 'tools.read({path:"README.md"})', 'ALL_TOOLS', 'describeTool("read")']) {
      const before = received.length;
      const result = await run(`return ${expression};`);
      expect(result.ok).toBe(false);
      if (expression.includes("tools")) expect(result.output).toContain("A workflow can only call agents.run and models.classify");
      expect(received.length).toBe(before);
    }
  });

  test("static and dynamic imports are unavailable", async () => {
    expect((await run('import fs from "node:fs";')).ok).toBe(false);
    const result = await run('return await import("node:fs");');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("dynamic import");
  });

  test("host errors keep their messages, are traced, and can be caught", async () => {
    const result = await run('try { await agents.run({agent:"orchestrator-editor",prompt:"work"}); } catch (e) { text(e.message); }\nreturn "recovered";', {
      host: async () => { throw new Error("Permission denied: /secret"); },
    });
    expect(result.ok).toBe(true);
    expect(result.output).toContain("Permission denied: /secret\nrecovered");
    expect(result.trace[0]).toMatchObject({ line: 1, ok: false, error: "Permission denied: /secret", result: null });
    expect(result.output).toContain("-> error: Permission denied: /secret");
  });

  test("uncaught failures retain partial output and write the complete trace", async () => {
    const result = await run('text("before");\nawait agents.run({agent:"orchestrator-scout",prompt:"work"});\nconsole.log("log before");\nthrow new Error("broken");');
    expect(result.ok).toBe(false);
    expect(result.output).toStartWith("Workflow failed in ");
    expect(result.output).toContain("before\n<console_output>\nlog before\n</console_output>");
    expect(result.output).toContain("Script error: broken");
    expect(records(result.traceFile).at(-1)).toMatchObject({ status: "failed" });
    expect(records(result.traceFile)[1]).toMatchObject({ line: 2, ok: true, result: null });
  });

  test("unawaited calls are recorded as abandoned and cannot append after return", async () => {
    const outstanding = Promise.withResolvers<Response>();
    const result = await run('models.classify({state:{},questions:{ready:{type:"bool",instructions:"Ready?"}}});\nreturn "finished";', {
      judge, fetchImpl: (async () => outstanding.promise) as typeof fetch,
    });
    expect(result.ok).toBe(true);
    expect(result.trace[0]).toMatchObject({ ok: false, error: "Abandoned when the script ended" });
    const saved = readFileSync(result.traceFile, "utf8");
    outstanding.resolve(Response.json({ answers: jevAnswers }));
    await Promise.resolve();
    expect(readFileSync(result.traceFile, "utf8")).toBe(saved);
  });

  test("small output budgets remain bounded, including a zero-length tail", async () => {
    for (let size = 1; size <= 80; size++) expect(truncate("x".repeat(500), size).length).toBeLessThanOrEqual(size);
    const result = await run('// @options: {"max_output_chars":18}\nreturn "x".repeat(9000);');
    expect(result.output.length).toBeLessThan(4800);
    expect(records(result.traceFile).at(-1).output).toContain("x".repeat(9000));
  });

  test("invalid helper input rejects inside the script and is recorded", async () => {
    const result = await run('try { await agents.run(null); } catch (error) { return error.message; }');
    expect(result.ok).toBe(true);
    expect(result.output).toContain("agents.run requires an argument object");
    expect(result.trace[0]).toMatchObject({ kind: "agent", ok: false, result: null });
  });

  test("truncation keeps output start/end, first/last trace lines, and full trace file", async () => {
    const result = await run([
      '// @options: {"max_output_chars":3600}',
      'text("START" + "x".repeat(9000) + "END");',
      'for (let i = 0; i < 80; i++) await agents.run({agent:"orchestrator-scout",prompt:"y".repeat(5000)});',
    ].join("\n"));
    expect(result.ok).toBe(true);
    expect(result.output.length).toBeLessThan(4800);
    expect(result.output).toContain("START");
    expect(result.output).toContain("END");
    expect(result.output).toContain("#1 L3");
    expect(result.output).toContain("#80 L3");
    expect(result.output).toContain("78 trace lines omitted");
    const log = records(result.traceFile);
    expect(log.length).toBe(82);
    expect(log.at(-1).output).toContain("x".repeat(9000));
    expect(log.slice(1, -1).every((entry) => entry.input.length <= 4000 && entry.output.length <= 4000)).toBe(true);
  });
});

describe("options and timeouts", () => {
  test("accepts a first-line JSON options comment and enforces exact limits", () => {
    expect(DEFAULT_OPTIONS.timeout_ms).toBe(1800000);
    expect(parseOptions("return 1")).toEqual(DEFAULT_OPTIONS);
    expect(parseOptions('// @options: {"timeout_ms":3000000,"max_output_chars":3600}\nreturn 1')).toEqual({ timeout_ms: 3_000_000, max_output_chars: 3600 });
    expect(parseOptions('// @options: {"timeout_ms":1,"max_output_chars":1}')).toEqual({ timeout_ms: 1, max_output_chars: 1 });
    for (const script of [
      '\n// @options: {}', 'text(1);\n// @options: {}', '// @options: {}\n// @options: {}',
      '// @options: {"timeout_ms":3000001}', '// @options: {"max_output_chars":3601}',
      '// @options: {"timeout_ms":0}', '// @options: {"max_output_chars":-1}',
      '// @options: {"timeout_ms":1.5}', '// @options: {"timeout_ms":null}', '// @options: {"max_output_chars":null}', '// @options: {"other":1}', '// @options: []', '// @options: nope',
    ]) expect(() => parseOptions(script)).toThrow();
  });

  test("rejected options still produce a failed result and a trace file", async () => {
    const result = await run('text("never");\n// @options: {}');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("@options must appear only on the first line");
    expect(records(result.traceFile).at(-1).status).toBe("failed");
  });

  test("deadline ends a hanging async script and keeps partial output", async () => {
    const result = await run('// @options: {"timeout_ms":500}\ntext("before hang");\nawait new Promise(() => {});');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("before hang");
    expect(result.output).toContain("Script timed out after 500 ms");
    expect(records(result.traceFile).at(-1).status).toBe("failed");
  });


  test("deadline also stops CPU loops after an awaited host call", async () => {
    const result = await run('// @options: {"timeout_ms":500}\nawait agents.run({agent:"orchestrator-scout",prompt:"work"});\ntext("resumed");\nwhile (true) {}');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("resumed");
    expect(result.output).toContain("Script timed out after 500 ms");
    expect(result.trace[0].ok).toBe(true);
  });

  test("deadline aborts outstanding judge work", async () => {
    let signal: AbortSignal | undefined;
    const result = await run('// @options: {"timeout_ms":500}\nawait models.classify({state:{},questions:{ready:{type:"bool",instructions:"Ready?"}}});', {
      judge, fetchImpl: (async (_url, init) => { signal = init?.signal as AbortSignal; return new Promise(() => {}); }) as typeof fetch,
    });
    expect(result.ok).toBe(false);
    expect(signal?.aborted).toBe(true);
    expect(result.output).toContain("Script timed out");
  });
});

describe("agents.run host bridge", () => {
  test("requires an explicit specialised agent before any host call", async () => {
    for (const agent of [undefined, "", "orchestrator-worker", "hya-scout", "bash"]) {
      const before = received.length;
      const result = await run(`return await agents.run(${JSON.stringify({ agent, prompt: "work" })});`);
      expect(result.ok).toBe(false);
      expect(result.output).toContain("agents.run agent");
      expect(received.length).toBe(before);
    }
    for (const agent of ["orchestrator-scout", "orchestrator-runner", "orchestrator-editor"]) {
      expect((await run(`return await agents.run({agent:${JSON.stringify(agent)},prompt:"work"});`)).ok).toBe(true);
    }
  });

  test("the host boundary refuses every non-coordination name and every non-channel read", async () => {
    const calls: any[] = [];
    const fakeHost: Host = async (method, params) => { calls.push({ method, params }); return { output: "allowed" }; };
    for (const name of ["bash", "write", "edit", "grep", "glob", "workflow", "tools.list", "send", "list_channel", "report", "arbitrary"]) {
      await expect(callHostTool(fakeHost, name, {}, new AbortController().signal)).rejects.toThrow("A workflow can only call agents.run and models.classify");
    }
    for (const path of ["README.md", "/tmp/file", "file:///tmp/file", "https://example.test", "Channel://DM", undefined, 2]) {
      await expect(callHostTool(fakeHost, "read", { path }, new AbortController().signal)).rejects.toThrow("channel reads");
    }
    expect(calls).toEqual([]);
    for (const name of ["task", "wait", "archive", "read"]) {
      await expect(callHostTool(checkedHost(fakeHost), name, name === "read" ? { path: "channel://DM-test" } : {}, new AbortController().signal)).resolves.toBe("allowed");
    }
    expect(calls.map(call => call.params.name)).toEqual(["task", "wait", "archive", "read"]);
  });

  test("result fields append the exact report instruction and parse into the agent trace", async () => {
    const calls: any[] = [];
    const result = await run('return await agents.run({agent:"orchestrator-runner",prompt:"Run tests",result:{passed:"Whether tests passed",command:"The command"}});', {
      host: async (_method, params) => {
        calls.push(params);
        if (params.name === "task") return { output: { metadata: { members: [{ member: "main/runner", session: "child", status: "running" }] } } };
        if (params.name === "wait") return { output: { metadata: { finished: [{ handle: "main/runner", outcome: "done", channel: "DM-result" }], running: [] } } };
        return { output: { output: '[main/runner] RESULT: {"passed":false,"command":"old"}\nEvidence\nRESULT: {"passed":true,"command":"bun test"}' } };
      },
    });
    expect(result.ok).toBe(true);
    expect(calls[0].input.tasks[0].prompt).toBe('Run tests\n\nEnd your report with one final line of the form RESULT: {...} — a single-line JSON object with exactly these fields:\n- passed: Whether tests passed\n- command: The command');
    expect(result.trace[0].result).toEqual({ passed: true, command: "bun test" });
    expect(records(result.traceFile)[1].result).toEqual({ passed: true, command: "bun test" });
  });

  for (const [report, requested, expected] of [
    ['No structured line', undefined, null],
    ['RESULT: not JSON', undefined, null],
    ['RESULT: null', undefined, null],
    ['RESULT: [1,2]', undefined, null],
    ['RESULT: 12', undefined, null],
    ['RESULT: "text"', undefined, null],
    ['RESULT: {"found":true}', { found: "found?", paths: "paths" }, null],
    ['RESULT: {"found":true}\nRESULT: invalid', undefined, null],
    ['[main/orchestrator-scout-x] RESULT: {"found":true}', { found: "found?" }, { found: true }],
    ['[main/orchestrator-scout-x] evidence\nRESULT: {"found":true}\ntrailing prose', undefined, { found: true }],
    ['RESULT: {"found":true,"extra":1}', { found: "found?" }, { found: true, extra: 1 }],
  ] as const) {
    test(`parses the last RESULT line: ${report}`, async () => {
      const result = await runAgent({ agent: "orchestrator-scout", prompt: "Find it", ...(requested ? { result: requested } : {}) }, async () => ({
        output: { metadata: { members: [{ member: "main/scout", session: "child", status: "done", summary: report }] } },
      }), new AbortController().signal);
      expect(result.report).toBe(report);
      expect(result.result).toEqual(expected);
    });
  }

  for (const status of ["done", "failed", "cancelled", "timeout"] as const) {
    test(`maps ${status} through real task/wait/read result envelopes`, async () => {
      const calls: Array<{ name: string; input: any }> = [];
      const handle = "main/orchestrator-editor-amiya";
      const fakeHost: Host = async (method, params) => {
        expect(method).toBe("tool.call");
        calls.push(params as any);
        switch (params.name) {
          case "task": return { output: { metadata: { members: [{ member: handle, session: "child-session", status: "running" }] } } };
          case "wait": return { output: { metadata: {
            woke_by: status === "timeout" ? "timeout" : "members",
            finished: status === "timeout" ? [] : [{ handle, session: "child-session", state: status === "cancelled" ? "archived" : "reported", outcome: status, channel: "DM-test", report_chars: 7000, report_truncated: true }],
            running: status === "timeout" ? [{ handle, state: "working" }] : [],
          } } };
          case "read": return { output: { title: "#DM-test · 1 message(s)", output: `[${handle}] ${"full report ".repeat(800)}`, metadata: { type: "channel", channel: "DM-test", count: 1, unreadRemaining: 0, warning: null } } };
          default: throw new Error(`unexpected ${params.name}`);
        }
      };
      const result = await runAgent({ agent: "orchestrator-editor", prompt: "repair exporter", model: "fake/worker", effort: "low", timeout_secs: 4 }, fakeHost, new AbortController().signal);
      expect(result).toMatchObject({ handle, session: "child-session", status });
      expect(calls[0].input).toEqual({ tasks: [{ subagent_type: "orchestrator-editor", prompt: "repair exporter", description: "repair exporter", model: "fake/worker", effort: "low" }] });
      expect(calls[1].input).toMatchObject({ targets: [handle], mode: "all" });
      expect(calls[1].input.timeout_secs).toBeLessThanOrEqual(4);
      if (status === "timeout") {
        expect(calls.length).toBe(2);
        expect(result.report).toBe("");
      } else {
        expect(calls[2].input).toEqual({ path: "channel://DM-test" });
        expect(result.report).toContain("full report ".repeat(800));
      }
    });
  }

  for (const ending of ["failure", "timeout", "completed", "late start", "archive failure", "unknown start"]) {
    test(`cleans up abandoned workers on ${ending}`, async () => {
      const calls: any[] = [];
      const task = Promise.withResolvers<any>();
      const wait = Promise.withResolvers<any>();
      const member = { member: "main/orchestrator-scout-x", session: "child", status: "running" };
      const spawned = { output: { metadata: { members: [member] } } };
      let startSignal: AbortSignal | undefined;
      const script = ending === "timeout"
        ? '// @options: {"timeout_ms":100}\nawait agents.run({agent:"orchestrator-scout",prompt:"Count files"});'
        : ending === "completed" || ending === "unknown start"
          ? 'agents.run({agent:"orchestrator-scout",prompt:"Count files"}); return 1;'
          : 'await Promise.all([agents.run({agent:"orchestrator-scout",prompt:"Count files"}), models.classify({state:{},questions:{}})]);';
      const result = await run(script, {
        host: async (method, params, signal) => {
          calls.push(params);
          if (params.name === "task") {
            startSignal = signal;
            if (ending === "unknown start") return task.promise;
            if (ending === "late start") {
              setTimeout(() => task.resolve(spawned), 30);
              return task.promise;
            }
            return spawned;
          }
          if (params.name === "wait") {
            expect(Number.isInteger(params.input.timeout_secs)).toBe(true);
            return wait.promise;
          }
          expect(params.name).toBe("archive");
          expect(params.input.target).toBe(member.member);
          expect(signal.aborted).toBe(false);
          if (ending === "archive failure") throw new Error("archive denied");
          return { output: {} };
        },
      });
      expect(result.ok).toBe(ending === "completed" || ending === "unknown start");
      expect(result.trace[0]).toMatchObject({ kind: "agent", abandoned: true, ok: false });
      if (ending === "unknown start") {
        expect(startSignal?.aborted).toBe(true);
        expect(result.output).toContain("abandoned (start unknown)");
        expect(calls.filter(call => call.name === "archive")).toHaveLength(0);
      } else {
        expect(result.trace[0]).toMatchObject({ handle: member.member, session: "child", archived: ending !== "archive failure" });
        expect(calls.filter(call => call.name === "archive")).toHaveLength(1);
        expect(result.output).toContain(ending === "archive failure"
          ? "abandoned and archive failed: archive denied, main/orchestrator-scout-x"
          : 'agents.run orchestrator-scout "Count files" -> abandoned and archived, main/orchestrator-scout-x');
      }
      expect(records(result.traceFile)[1]).toMatchObject(result.trace[0]);
      const saved = JSON.stringify(result.trace);
      task.resolve(spawned);
      wait.resolve({ output: { metadata: { finished: [], running: [], woke_by: "timeout" } } });
      await new Promise(resolve => setImmediate(resolve));
      expect(JSON.stringify(result.trace)).toBe(saved);
    }, 10000);
  }

  test("notifies identity before waiting and rounds fractional wait time up", async () => {
    let identity: any;
    const result = await runAgent({ agent: "orchestrator-editor", prompt: "work", timeout_secs: 0.1 }, async (_method, params) => {
      if (params.name === "task") return { output: { metadata: { members: [{ member: "main/worker", session: "child", status: "running" }] } } };
      expect(identity).toMatchObject({ handle: "main/worker", session: "child" });
      expect(params.input.timeout_secs).toBe(1);
      return { output: { metadata: { finished: [], running: [], woke_by: "timeout" } } };
    }, new AbortController().signal, value => { identity = { ...value }; });
    expect(result.status).toBe("timeout");
  });

  test("mail/stall wakes keep waiting; already_finished is terminal; handle read works without channel", async () => {
    let waits = 0;
    const calls: string[] = [];
    const result = await runAgent({ agent: "orchestrator-scout", prompt: "find it" }, async (_method, params) => {
      calls.push(params.name);
      if (params.name === "task") return { output: { metadata: { members: [{ member: "main/orchestrator-scout-amiya", session: "child", status: "running" }] } } };
      if (params.name === "read") {
        expect(params.input.path).toBe("channel://main/orchestrator-scout-amiya");
        return { output: { output: "[warning] normalized channel\n[main/orchestrator-scout-amiya] found" } };
      }
      const metadata = ++waits < 3
        ? { woke_by: waits === 1 ? "mail" : "stalled", finished: [], running: [{ handle: "main/orchestrator-scout-amiya", state: "idle" }] }
        : { woke_by: "nothing_to_wait_for", finished: [], already_finished: [{ handle: "main/orchestrator-scout-amiya", outcome: "done" }], running: [] };
      return { output: { metadata } };
    }, new AbortController().signal);
    expect(result.status).toBe("done");
    expect(calls).toEqual(["task", "wait", "wait", "wait", "read"]);
  });

  test("spawn failures resolve failed without waiting; timeouts are bounded", async () => {
    const failure = await runAgent({ agent: "orchestrator-editor", prompt: "work" }, async () => ({ output: { metadata: { members: [{ member: "", session: "", status: "error", summary: "unknown_agent_id" }] } } }), new AbortController().signal);
    expect(failure).toMatchObject({ status: "failed", report: "unknown_agent_id" });
    await expect(runAgent({ agent: "orchestrator-editor", prompt: "work", timeout_secs: 1801 }, host, new AbortController().signal)).rejects.toThrow("timeout_secs");
  });

  test("parallel agents keep their handles, waits, and reports independent", async () => {
    const result = await run('return await Promise.all([agents.run({agent:"orchestrator-editor",prompt:"first"}), agents.run({agent:"orchestrator-editor",prompt:"second"})]);', {
      host: async (method, params) => {
        if (params.name === "task") {
          const label = params.input.tasks[0].prompt;
          return { output: { metadata: { members: [{ member: `main/${label}`, session: `session-${label}`, status: "running" }] } } };
        }
        if (params.name === "wait") return { output: { metadata: { woke_by: "members", finished: [{ handle: params.input.targets[0], outcome: "done", channel: `DM-${params.input.targets[0].split("/")[1]}` }], running: [] } } };
        return { output: { output: `report from ${params.input.path}` } };
      },
    });
    expect(result.ok).toBe(true);
    expect(result.trace.map(entry => entry.handle)).toEqual(["main/first", "main/second"]);
    expect(result.output).toContain('"handle":"main/first","session":"session-first","status":"done","report":"report from channel://DM-first"');
    expect(result.output).toContain('"handle":"main/second","session":"session-second","status":"done","report":"report from channel://DM-second"');
  });

  test("script trace has one agent entry with source line and worker identity", async () => {
    const result = await run('text("start");\nreturn await agents.run({agent:"orchestrator-editor",prompt:"fix exporter",description:"repair exporter"});', {
      host: async (method, params) => {
        if (params.name === "task") return { output: { metadata: { members: [{ member: "main/orchestrator-editor-amiya", session: "child", status: "running" }] } } };
        if (params.name === "wait") return { output: { metadata: { finished: [{ handle: "main/orchestrator-editor-amiya", outcome: "done", channel: "DM-test" }], running: [], woke_by: "members" } } };
        return { output: { output: "[main/orchestrator-editor-amiya] done" } };
      },
    });
    expect(result.ok).toBe(true);
    expect(result.trace).toHaveLength(1);
    expect(result.trace[0]).toMatchObject({ kind: "agent", line: 2, handle: "main/orchestrator-editor-amiya", session: "child" });
    expect(result.output).toContain('L2 agents.run orchestrator-editor "repair exporter" -> done, main/orchestrator-editor-amiya');
  });
});

describe("workflow inputs and persistent store", () => {
  const store = join(directory, "store");
  const metadata = (name: string) => JSON.parse(readFileSync(join(store, "workflows", `${name}.json`), "utf8"));
  const saved = (name: string) => readFileSync(join(store, "workflows", `${name}.js`), "utf8");

  test("plain runs expose args, preserve their trace input, and start with Workflow", async () => {
    const script = "return args.request;";
    const result = await workflow({ script, args: { request: "hello" } }, { configDir: store });
    expect(result.ok).toBe(true);
    expect(result.output).toStartWith("Workflow completed in ");
    expect(result.output).toContain("\nhello\n");
    expect(result.output).not.toContain("saved as");
    expect(records(result.output.match(/trace file: (.+)$/)![1])[0]).toMatchObject({ script, args: { request: "hello" }, stored: null });
  });

  test("save only completed scripts; replace source unchanged; run with fresh args and update metadata", async () => {
    const source = '// Return the request\nif (args.fail) throw new Error("failed request");\nreturn args.request;\n';
    const failed = await workflow({ script: 'throw new Error("no save");', save_as: "never-saved" }, { configDir: store });
    expect(failed.ok).toBe(false);
    expect(failed.output).toStartWith("Workflow failed in ");
    expect(failed.output).not.toContain("saved as");
    expect(records(failed.output.match(/trace file: (.+)$/)![1])[0].stored).toBeNull();
    expect(existsSync(join(store, "workflows", "never-saved.js"))).toBe(false);

    const first = await workflow({ script: source, args: { request: "first" }, save_as: "return-request" }, { configDir: store });
    expect(first.ok).toBe(true);
    expect(first.output).toStartWith("Workflow completed in ");
    expect(first.output).toContain("\nsaved as return-request\n");
    expect(saved("return-request")).toBe(source);
    const initial = metadata("return-request");
    expect(initial).toEqual({ name: "return-request", description: "Return the request", saved_ms: expect.any(Number), runs: 1, last_run_ms: expect.any(Number), last_status: "completed" });
    expect(records(first.output.match(/trace file: (.+)$/)![1])[0]).toMatchObject({ script: source, args: { request: "first" }, stored: "return-request" });

    const second = await workflow({ run: "return-request", args: { request: "second" } }, { configDir: store, session: "another-session" });
    expect(second.output).toStartWith("Workflow return-request completed in ");
    expect(second.output).toContain("\nsecond\n");
    expect(second.output).not.toContain("saved as");
    expect(metadata("return-request")).toMatchObject({ saved_ms: initial.saved_ms, runs: 2, last_status: "completed" });
    expect(metadata("return-request").last_run_ms).toBeGreaterThanOrEqual(initial.last_run_ms);
    expect(records(second.output.match(/trace file: (.+)$/)![1])[0]).toMatchObject({ script: source, args: { request: "second" }, stored: "return-request" });

    const failedRun = await workflow({ run: "return-request", args: { fail: true } }, { configDir: store });
    expect(failedRun.ok).toBe(false);
    expect(failedRun.output).toStartWith("Workflow return-request failed in ");
    expect(metadata("return-request")).toMatchObject({ runs: 3, last_status: "failed" });
    expect(saved("return-request")).toBe(source);
    await workflow({ script: 'throw new Error("do not replace");', save_as: "return-request" }, { configDir: store });
    expect(saved("return-request")).toBe(source);
    expect(metadata("return-request")).toMatchObject({ runs: 3, last_status: "failed" });

    const replacement = '// Replacement\nreturn {value: args.value};';
    await workflow({ script: replacement, save_as: "return-request" }, { configDir: store });
    expect(saved("return-request")).toBe(replacement);
    expect(metadata("return-request")).toMatchObject({ description: "Replacement", runs: 1, last_status: "completed" });
    expect((await workflow({ run: "return-request", args: { value: 42 } }, { configDir: store })).output).toContain('{"value":42}');
  });

  test("list returns names, first-comment descriptions, saved times and run counts without executing", async () => {
    const local = join(directory, "listing");
    expect((await workflow({ list: true }, { configDir: local })).output).toBe("Stored workflows:\n[]");
    await workflow({ script: '// Description\nreturn 1;', save_as: "comment" }, { configDir: local });
    await workflow({ script: '// @options: {"timeout_ms":500}\n// Not the first line\nreturn 2;', save_as: "options" }, { configDir: local });
    await workflow({ script: 'return 3;\n// Later comment', save_as: "no-comment" }, { configDir: local });
    await workflow({ run: "comment" }, { configDir: local });
    const before = received.length;
    const traceFiles = readdirSync(join(local, "traces", "test-session"));
    const listed = await workflow({ list: true }, { configDir: local });
    expect(listed.ok).toBe(true);
    expect(JSON.parse(listed.output.slice("Stored workflows:\n".length))).toEqual([
      { name: "comment", description: "Description", saved_ms: expect.any(Number), runs: 2 },
      { name: "no-comment", description: "", saved_ms: expect.any(Number), runs: 1 },
      { name: "options", description: "", saved_ms: expect.any(Number), runs: 1 },
    ]);
    expect(received.length).toBe(before);
    expect(readdirSync(join(local, "traces", "test-session"))).toEqual(traceFiles);
    await expect(workflow({ run: "unknown" }, { configDir: local })).rejects.toThrow("Stored workflows: comment, no-comment, options");
    await expect(workflow({ run: "unknown" }, { configDir: join(directory, "empty") })).rejects.toThrow("Stored workflows: (none)");
  });

  test("rejects invalid names before execution", async () => {
    for (const name of ["", "../escape", "Upper", "two words", "-start", "a".repeat(65), 12]) {
      for (const input of [{ script: 'throw new Error("must not run");', save_as: name }, { run: name }]) {
        await expect(workflow(input, { configDir: store })).rejects.toThrow(typeof name === "number" && "run" in input ? "workflow input must be exactly one of" : "must match");
      }
    }
    expect((await workflow({ script: "return 1;", save_as: "a".repeat(64) }, { configDir: store })).ok).toBe(true);
  });

  test("wrong combinations identify all three forms", async () => {
    for (const input of [null, [], {}, { script: 1 }, { run: 2 }, { list: false }, { script: "return 1", run: "x" }, { script: "return 1", list: true }, { run: "x", save_as: "y" }, { list: true, args: {} }, { args: {} }, { script: "return 1", extra: true }, { script: "return 1", args: [] }, { script: "return 1", args: null }, { run: "x", args: "wrong" }]) {
      await expect(workflow(input, { configDir: store })).rejects.toThrow("{script: string, args?: object, save_as?: string}, {run: string, args?: object}, or {list: true}");
    }
  });
});

describe("judge configuration and classification", () => {
  test("config defaults, key file, and validation", () => {
    expect(parseJudgeConfig({ agents: {} })).toBeNull();
    expect(judge).toEqual({ endpoint: "https://api.typesafe.ai/v1/systemone", model: "jev-1.14.0", apiKey: "test-key", timeoutMs: 20000 });
    expect(parseJudgeConfig({ judge: { api_key_file: "/test/key", model: "fake/judge", timeout_ms: 123 } }, () => " key\n")).toMatchObject({ apiKey: "key", timeoutMs: 123 });
    for (const raw of [null, { judge: {} }, { judge: { api_key: "x", api_key_file: "/key" } }, { judge: { api_key_file: "relative" } }, { judge: { api_key: " " } }, { judge: { api_key: "x", typo: 1 } }, { judge: { api_key: "x", timeout_ms: 0 } }]) {
      expect(() => parseJudgeConfig(raw)).toThrow();
    }
  });

  test("fake HTTP request maps all three question and answer types", async () => {
    const fetchImpl = (async (url, init) => {
      expect(url).toBe(judge.endpoint);
      expect(init?.method).toBe("POST");
      expect(init?.headers).toMatchObject({ authorization: "Bearer test-key" });
      const body = JSON.parse(init!.body as string);
      expect(body.model).toBe("jev-1.14.0");
      expect(body.state).toEqual(["source", { n: 3 }]);
      expect(body.questions.ready).toEqual({ ...questions.ready, type: "noul" });
      expect(body.questions.kind).toEqual(questions.kind);
      expect(body.questions.quality).toEqual(questions.quality);
      return Response.json({ answers: jevAnswers });
    }) as typeof fetch;
    const result = await classify(judge, { state: ["source", { n: 3 }], questions }, new AbortController().signal, fetchImpl);
    expect(result.answers).toEqual({ ready: { type: "bool", probability: 0.9 }, kind: jevAnswers.kind, quality: jevAnswers.quality });
    expect(buildJevRequest(judge, { state: null, questions: { ready: { type: "bool", instructions: "yes?" } } })).toMatchObject({ state: null });
  });

  test("no judge, HTTP failures, malformed answers, and fetch errors reject clearly", async () => {
    const signal = new AbortController().signal;
    await expect(classify(null, { state: {}, questions }, signal, fakeFetch)).rejects.toThrow("No judge configured");
    await expect(classify(judge, { state: {}, questions }, signal, (async () => new Response("denied", { status: 401 })) as typeof fetch)).rejects.toThrow("HTTP 401");
    await expect(classify(judge, { state: {}, questions }, signal, (async () => Response.json({ answers: {} })) as typeof fetch)).rejects.toThrow("no answer for ready");
    await expect(classify(judge, { state: {}, questions }, signal, (async () => { throw new Error("network offline"); }) as typeof fetch)).rejects.toThrow("network offline");
    expect(() => buildJevRequest(judge, { state: {}, questions: { bad: { type: "wrong" } as any } })).toThrow();
  });

  test("a single classification uses the specified compact trace format", async () => {
    const result = await run(`return await models.classify({state:"sample",questions:${JSON.stringify({kind: questions.kind})}});`, { judge, fetchImpl: fakeFetch });
    expect(result.ok).toBe(true);
    expect(result.output).toContain("#1 L1 models.classify kind -> repair (p=0.97)");
  });

  test("classify trace maps source line, typed answers and decision summary", async () => {
    const result = await run(`const state = {diff:"fix"};\nreturn await models.classify({state,questions:${JSON.stringify(questions)}});`, { judge, fetchImpl: fakeFetch });
    expect(result.ok).toBe(true);
    expect(result.trace[0]).toMatchObject({ kind: "classify", line: 2, name: "models.classify", ok: true });
    expect(result.output).toContain("kind -> repair (p=0.97)");
    expect(result.output).toContain('"type":"bool","probability":0.9');
    const absent = await run('return await models.classify({state:{},questions:{}});');
    expect(absent.ok).toBe(false);
    expect(absent.output).toContain("No judge configured");
  });
});

describe("request-scoped stdio protocol", () => {
  test("capability errors reject with host text and replies correlate out of order", async () => {
    const frames: any[] = [];
    const client = new CapabilityClient((frame) => frames.push(frame));
    const signal = new AbortController().signal;
    const first = client.call("token", "session", "call", "tool.call", { name: "echo" }, signal);
    const second = client.call("token", "session", "call", "tool.call", { name: "wait" }, signal);
    expect(client.accept({ id: 1, method: "initialize" })).toBe(false);
    client.accept({ id: frames[1].id, result: { output: {} } });
    client.accept({ id: frames[0].id, error: { code: -32003, message: "tool exploded" } });
    await expect(first).rejects.toThrow("tool exploded");
    expect(await second).toEqual({ output: {} });
  });

  test("real runtime child initializes, interleaves a host call, replies, and shuts down", async () => {
    const child = Bun.spawn([process.execPath, "run", join(import.meta.dir, "runtime.ts")], {
      cwd: import.meta.dir, stdin: "pipe", stdout: "pipe", stderr: "pipe",
      env: { HYA_BUNDLE_CONFIG_DIR: directory },
    });
    async function* messages() {
      let buffer = "";
      const decoder = new TextDecoder();
      for await (const chunk of child.stdout) {
        buffer += decoder.decode(chunk, { stream: true });
        let newline: number;
        while ((newline = buffer.indexOf("\n")) >= 0) {
          yield JSON.parse(buffer.slice(0, newline));
          buffer = buffer.slice(newline + 1);
        }
      }
    }
    const incoming = messages();
    const send = (frame: any) => child.stdin.write(JSON.stringify({ jsonrpc: "2.0", ...frame }) + "\n");
    const next = async () => {
      const frame = await incoming.next();
      if (frame.done) throw new Error(`child exited: ${await new Response(child.stderr).text()}`);
      return frame.value;
    };
    try {
      send({ id: 1, method: "initialize", params: { protocol_version: 1, host: { name: "test", version: "1" } } });
      expect(await next()).toEqual({ jsonrpc: "2.0", id: 1, result: INITIALIZE_RESULT });
      expect(INITIALIZE_RESULT.tools[0]).toMatchObject({ name: "workflow", inputSchema: { type: "object", oneOf: expect.any(Array) } });
      send({ id: 2, method: "tool/call", params: { tool: "workflow", session: "stdio-session", call: "stdio-call", host_capability: "opaque-token", input: { script: 'return await agents.run({agent:"orchestrator-scout",prompt:args.request});', args: {request:"wire"}, save_as: "wire-check" } } });
      const call = await next();
      expect(call).toMatchObject({ method: "host/capability", params: { capability: "opaque-token", session: "stdio-session", call: "stdio-call", method: "tool.call", params: { name: "task", input: { tasks: [{subagent_type:"orchestrator-scout",prompt:"wire"}] } } } });
      send({ id: 3, method: "initialize", params: {} });
      expect((await next()).id).toBe(3);
      send({ id: call.id, result: { output: { metadata: { members: [{member:"main/scout",session:"child",status:"done",summary:'RESULT: {"wire":true}'}] } } } });
      const reply = await next();
      expect(reply).toMatchObject({ id: 2, result: { ok: true } });
      expect(reply.result.output).toStartWith("Workflow completed in ");
      expect(reply.result.output).toContain("saved as wire-check");
      expect(reply.result.output).toContain('"result":{"wire":true}');
      expect(reply.result.output).toContain("#1 L1 agents.run");
      expect(records(join(directory, "traces", "stdio-session", "stdio-call.jsonl"))[1]).toMatchObject({ ok: true, result: {wire:true} });
      send({ id: 5, method: "tool/call", params: { tool: "workflow", session: "stdio-session", call: "invalid", host_capability: "opaque-token", input: {script:"return 1",list:true} } });
      expect(await next()).toMatchObject({ id:5,error:{code:-32602,message:expect.stringContaining("{list: true}")} });
      send({ id: 6, method: "tool/call", params: { tool: "workflow", session: "stdio-session", call: "listing", host_capability: "opaque-token", input: {list:true} } });
      expect(await next()).toMatchObject({ id:6,result:{ok:true,output:expect.stringContaining("wire-check")} });
      send({ id: 4, method: "shutdown", params: {} });
      expect(await next()).toEqual({ jsonrpc: "2.0", id: 4, result: {} });
      expect(await child.exited).toBe(0);
      expect(await new Response(child.stderr).text()).toBe("");
    } finally {
      child.stdin.end();
      child.kill();
      await child.exited;
    }
  }, 5000);
});
