import { afterAll, describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import {
  CapabilityClient, DEFAULT_OPTIONS, INITIALIZE_RESULT, buildJevRequest, classify,
  executeScript, parseJudgeConfig, parseOptions, runAgent, truncate,
  type ClassifyInput, type Host, type RunContext, type ToolSchema,
} from "./runtime.ts";

const directory = mkdtempSync(join(import.meta.dir, ".runtime-test-"));
afterAll(() => rmSync(directory, { recursive: true, force: true }));
let nextCall = 0;
const schema = (name: string): ToolSchema => ({ name, description: `${name} description`, input_schema: { type: "object" } });
const schemas = [schema("echo"), schema("file-read")];
const host: Host = async (method, params) => method === "tools.list" ? { tools: schemas } : { output: params.input };
const run = (script: string, overrides: Partial<RunContext> = {}) => executeScript(script, {
  session: "test-session", call: `call-${++nextCall}`, configDir: directory, host, ...overrides,
});
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
    const first = Promise.withResolvers<unknown>();
    const second = Promise.withResolvers<unknown>();
    const bothStarted = Promise.withResolvers<void>();
    const calls: string[] = [];
    let listCalls = 0;
    const pending = run([
      'const found = ALL_TOOLS.filter(t => t.name.includes("echo"));',
      'text(found.map(t => t.name));',
      'const a = tools.echo({order: 1});',
      'const b = tools.file_read({order: 2});',
      'const values = await Promise.all([a, b]);',
      'console.log("kept", values.length);',
      'return values.filter(v => v.keep).map(v => v.value);',
    ].join("\n"), { host: async (method, params) => {
      if (method === "tools.list") { listCalls++; return { tools: schemas }; }
      calls.push(params.name);
      if (calls.length === 2) bothStarted.resolve();
      return { output: await (params.name === "echo" ? first.promise : second.promise) };
    } });
    await bothStarted.promise;
    const a = { keep: true, value: "kept".repeat(1500) };
    const b = { keep: false, value: "discard" };
    second.resolve(b);
    first.resolve(a);
    const result = await pending;
    expect(result.ok).toBe(true);
    expect(listCalls).toBe(1);
    expect(calls).toEqual(["echo", "file-read"]);
    expect(result.trace.map(({ seq, line, name }) => ({ seq, line, name }))).toEqual([
      { seq: 1, line: 3, name: "echo" }, { seq: 2, line: 4, name: "file-read" },
    ]);
    expect(result.output).toContain('["echo"]');
    expect(result.output).toContain('<console_output>\nkept 2\n</console_output>');
    expect(result.output).toContain(`#1 L3 tools.echo {"order":1} -> ok, ${JSON.stringify(a).length} chars`);
    expect(result.output.indexOf("#1")).toBeLessThan(result.output.indexOf("#2"));
    expect(result.trace[0].output!.length).toBe(4000);
    const log = records(result.traceFile);
    expect(log[0].script).toContain("const a = tools.echo");
    expect(log[0].options).toEqual(DEFAULT_OPTIONS);
    expect(log.at(-1).status).toBe("completed");
    expect(log.at(-1).output).toContain("kept".repeat(1500));
    expect(log.at(-1).output).not.toContain("discard");
  });

  test("only specified effects and standard JavaScript globals are available", async () => {
    const result = await run(`return {
      require: typeof require, process: typeof process, fetch: typeof fetch,
      Bun: typeof Bun, setTimeout: typeof setTimeout, setInterval: typeof setInterval,
      consoleWarn: typeof console.warn, Promise: typeof Promise, JSON: typeof JSON,
      tools: ALL_TOOLS, schema: describeTool("echo"), missing: describeTool("absent")
    };`);
    expect(result.ok).toBe(true);
    for (const key of ["require", "process", "fetch", "Bun", "setTimeout", "setInterval", "consoleWarn"]) {
      expect(result.output).toContain(`"${key}":"undefined"`);
    }
    expect(result.output).toContain('"Promise":"function"');
    expect(result.output).toContain('"JSON":"object"');
    expect(result.output).toContain('"schema":{"name":"echo","description":"echo description","input_schema":{"type":"object"}}');
    expect(result.output).not.toContain('"missing":');
  });

  test("static and dynamic imports are unavailable", async () => {
    expect((await run('import fs from "node:fs";')).ok).toBe(false);
    const result = await run('return await import("node:fs");');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("dynamic import");
  });

  test("a tool error keeps its message, is traced, and can be caught", async () => {
    const result = await run('try { await tools.echo({}); } catch (e) { text(e.message); }\nreturn "recovered";', {
      host: async (method) => {
        if (method === "tools.list") return { tools: schemas };
        throw new Error("Permission denied: /secret");
      },
    });
    expect(result.ok).toBe(true);
    expect(result.output).toContain("Permission denied: /secret\nrecovered");
    expect(result.trace[0]).toMatchObject({ line: 1, ok: false, error: "Permission denied: /secret" });
    expect(result.output).toContain("-> error: Permission denied: /secret");
  });

  test("uncaught failures retain partial output and write the complete trace", async () => {
    const result = await run('text("before");\nawait tools.echo({n: 1});\nconsole.log("log before");\nthrow new Error("broken");');
    expect(result.ok).toBe(false);
    expect(result.output).toStartWith("Script failed in ");
    expect(result.output).toContain("before\n<console_output>\nlog before\n</console_output>");
    expect(result.output).toContain("Script error: broken");
    expect(records(result.traceFile).at(-1)).toMatchObject({ status: "failed" });
    expect(records(result.traceFile)[1]).toMatchObject({ line: 2, ok: true });
  });

  test("unawaited calls are recorded as abandoned and cannot append after return", async () => {
    const outstanding = Promise.withResolvers<any>();
    const result = await run('tools.echo({});\nreturn "finished";', {
      host: async (method) => method === "tools.list" ? { tools: schemas } : outstanding.promise,
    });
    expect(result.ok).toBe(true);
    expect(result.trace[0]).toMatchObject({ ok: false, error: "Abandoned when the script ended" });
    const saved = readFileSync(result.traceFile, "utf8");
    outstanding.resolve({ output: "late" });
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
    expect(result.trace[0]).toMatchObject({ kind: "agent", ok: false });
  });

  test("truncation keeps output start/end, first/last trace lines, and full trace file", async () => {
    const result = await run([
      '// @options: {"max_output_chars":3600}',
      'text("START" + "x".repeat(9000) + "END");',
      'for (let i = 0; i < 80; i++) await tools.echo({i, data: "y".repeat(5000)});',
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
    const result = await run('// @options: {"timeout_ms":500}\nawait tools.echo({});\ntext("resumed");\nwhile (true) {}');
    expect(result.ok).toBe(false);
    expect(result.output).toContain("resumed");
    expect(result.output).toContain("Script timed out after 500 ms");
    expect(result.trace[0].ok).toBe(true);
  });

  test("deadline also covers tool discovery and aborts outstanding host work", async () => {
    let signal: AbortSignal | undefined;
    const result = await run('// @options: {"timeout_ms":500}\nreturn 1;', {
      host: async (_method, _params, receivedSignal) => { signal = receivedSignal; return new Promise(() => {}); },
    });
    expect(result.ok).toBe(false);
    expect(signal?.aborted).toBe(true);
    expect(result.output).toContain("Script timed out");
  });
});

describe("agents.run host bridge", () => {
  for (const status of ["done", "failed", "cancelled", "timeout"] as const) {
    test(`maps ${status} through real task/wait/read result envelopes`, async () => {
      const calls: Array<{ name: string; input: any }> = [];
      const handle = "main/orchestrator-worker-amiya";
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
      const result = await runAgent({ prompt: "repair exporter", model: "fake/worker", effort: "low", timeout_secs: 4 }, fakeHost, new AbortController().signal);
      expect(result).toMatchObject({ handle, session: "child-session", status });
      expect(calls[0].input).toEqual({ tasks: [{ subagent_type: "orchestrator-worker", prompt: "repair exporter", description: "repair exporter", model: "fake/worker", effort: "low" }] });
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

  test("mail/stall wakes keep waiting; already_finished is terminal; handle read works without channel", async () => {
    let waits = 0;
    const calls: string[] = [];
    const result = await runAgent({ agent: "hya-scout", prompt: "find it" }, async (_method, params) => {
      calls.push(params.name);
      if (params.name === "task") return { output: { metadata: { members: [{ member: "main/hya-scout-amiya", session: "child", status: "running" }] } } };
      if (params.name === "read") {
        expect(params.input.path).toBe("channel://main/hya-scout-amiya");
        return { output: { output: "[warning] normalized channel\n[main/hya-scout-amiya] found" } };
      }
      const metadata = ++waits < 3
        ? { woke_by: waits === 1 ? "mail" : "stalled", finished: [], running: [{ handle: "main/hya-scout-amiya", state: "idle" }] }
        : { woke_by: "nothing_to_wait_for", finished: [], already_finished: [{ handle: "main/hya-scout-amiya", outcome: "done" }], running: [] };
      return { output: { metadata } };
    }, new AbortController().signal);
    expect(result.status).toBe("done");
    expect(calls).toEqual(["task", "wait", "wait", "wait", "read"]);
  });

  test("spawn failures resolve failed without waiting; timeouts are bounded", async () => {
    const failure = await runAgent({ prompt: "work" }, async () => ({ output: { metadata: { members: [{ member: "", session: "", status: "error", summary: "unknown_agent_id" }] } } }), new AbortController().signal);
    expect(failure).toMatchObject({ status: "failed", report: "unknown_agent_id" });
    await expect(runAgent({ prompt: "work", timeout_secs: 1801 }, host, new AbortController().signal)).rejects.toThrow("timeout_secs");
  });

  test("parallel agents keep their handles, waits, and reports independent", async () => {
    const result = await run('return await Promise.all([agents.run({prompt:"first"}), agents.run({prompt:"second"})]);', {
      host: async (method, params) => {
        if (method === "tools.list") return { tools: [] };
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
    const result = await run('text("start");\nreturn await agents.run({prompt:"fix exporter",description:"repair exporter"});', {
      host: async (method, params) => {
        if (method === "tools.list") return { tools: [] };
        if (params.name === "task") return { output: { metadata: { members: [{ member: "main/orchestrator-worker-amiya", session: "child", status: "running" }] } } };
        if (params.name === "wait") return { output: { metadata: { finished: [{ handle: "main/orchestrator-worker-amiya", outcome: "done", channel: "DM-test" }], running: [], woke_by: "members" } } };
        return { output: { output: "[main/orchestrator-worker-amiya] done" } };
      },
    });
    expect(result.ok).toBe(true);
    expect(result.trace).toHaveLength(1);
    expect(result.trace[0]).toMatchObject({ kind: "agent", line: 2, handle: "main/orchestrator-worker-amiya", session: "child" });
    expect(result.output).toContain('L2 agents.run orchestrator-worker "repair exporter" -> done, main/orchestrator-worker-amiya');
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
    const second = client.call("token", "session", "call", "tools.list", {}, signal);
    expect(client.accept({ id: 1, method: "initialize" })).toBe(false);
    client.accept({ id: frames[1].id, result: { tools: [] } });
    client.accept({ id: frames[0].id, error: { code: -32003, message: "tool exploded" } });
    await expect(first).rejects.toThrow("tool exploded");
    expect(await second).toEqual({ tools: [] });
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
      expect(INITIALIZE_RESULT.tools[0].inputSchema).toMatchObject({ type: "object", required: ["script"], properties: { script: { type: "string" } } });
      send({ id: 2, method: "tool/call", params: { tool: "code", session: "stdio-session", call: "stdio-call", host_capability: "opaque-token", input: { script: 'return await tools.echo({value:"wire"});' } } });
      const list = await next();
      expect(list).toMatchObject({ method: "host/capability", params: { capability: "opaque-token", session: "stdio-session", call: "stdio-call", method: "tools.list", params: {} } });
      send({ id: 3, method: "initialize", params: {} });
      expect((await next()).id).toBe(3);
      send({ id: list.id, result: { tools: [schema("echo")] } });
      const call = await next();
      expect(call).toMatchObject({ method: "host/capability", params: { method: "tool.call", params: { name: "echo", input: { value: "wire" } } } });
      send({ id: call.id, result: { output: { echoed: "wire" } } });
      const reply = await next();
      expect(reply).toMatchObject({ id: 2, result: { ok: true } });
      expect(reply.result.output).toContain('{"echoed":"wire"}');
      expect(reply.result.output).toContain("#1 L1 tools.echo");
      expect(records(join(directory, "traces", "stdio-session", "stdio-call.jsonl"))[1].ok).toBe(true);
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
