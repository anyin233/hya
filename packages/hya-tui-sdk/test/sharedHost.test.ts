// The shared extension host (src/main.ts) as the TUI drives it: one child
// process, the host channel on stdin/stdout, this test playing the TUI
// (including answering the host's fs requests).
import { afterEach, describe, expect, test } from "bun:test";
import { VM_STOPPED_ERROR_CODE } from "../src/protocol";
import { join } from "node:path";
import type { Subprocess } from "bun";

const fixture = (name: string) => join(import.meta.dir, "fixtures", name);
type Frame = { id: string | number; method?: string; params?: Record<string, unknown>; result?: unknown; error?: { code: number; message: string } };

/** A running host and a client for its channel. */
function host(serveFs: (method: string, params: Record<string, unknown>) => Promise<unknown> = async () => { throw new Error("no fs") }) {
  const child: Subprocess<"pipe", "pipe", "pipe"> = Bun.spawn([process.execPath, join(import.meta.dir, "../src/main.ts")], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  const waiting = new Map<string | number, PromiseWithResolvers<Frame>>();
  let nextId = 0;
  const write = (frame: object) => { child.stdin.write(`${JSON.stringify(frame)}\n`); };
  void (async () => {
    let buffer = "";
    for await (const chunk of child.stdout) {
      buffer += new TextDecoder().decode(chunk);
      let newline: number;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const frame = JSON.parse(buffer.slice(0, newline)) as Frame;
        buffer = buffer.slice(newline + 1);
        if (frame.method) {
          void serveFs(frame.method, frame.params ?? {}).then(
            (result) => write({ jsonrpc: "2.0", id: frame.id, result: result ?? null }),
            (error: Error) => write({ jsonrpc: "2.0", id: frame.id, error: { code: -32000, message: error.message } }),
          );
        } else {
          waiting.get(frame.id)?.resolve(frame);
          waiting.delete(frame.id);
        }
      }
    }
  })();
  const request = (method: string, params: unknown): Promise<Frame> => {
    const id = ++nextId;
    const waiter = Promise.withResolvers<Frame>();
    waiting.set(id, waiter);
    write({ jsonrpc: "2.0", id, method, params });
    return waiter.promise;
  };
  let requestId = 0;
  const call = async (ext: string, method: string, params: unknown) => {
    const frame = await request("ext/call", { ext, request: { jsonrpc: "2.0", id: ++requestId, method, params } });
    return frame.error ? { error: frame.error } : (frame.result as { response: { result?: unknown; error?: { code: number; message: string } } }).response;
  };
  const load = async (ext: string, entry: string, permissions: string[] = [], jit = false) => {
    const loaded = await request("host/load", { ext, entry: fixture(entry), permissions, jit });
    if (loaded.error) return loaded.error.message;
    await call(ext, "tui/initialize", { api_version: 1, sdk_version: "1.0.0", extension_id: ext, permissions });
    await call(ext, "tui/activate", { context: { terminal: { columns: 80, rows: 24 } } });
    return undefined;
  };
  const render = (ext: string, id: string) => call(ext, "tui/render", { surface: { kind: "panel", id }, width: 20, height: 2, now: 0 });
  return { child, request, call, load, render };
}

let running: Subprocess | undefined;
afterEach(() => { running?.kill(); running = undefined; });

test("one host loads several extensions, answers each, and unloads one", async () => {
  const h = host();
  running = h.child;
  expect(await Promise.all(["a", "b", "c"].map((ext) => h.load(ext, "hello.ts")))).toEqual([undefined, undefined, undefined]);
  for (const ext of ["a", "b", "c"]) expect((await h.render(ext, "hello")).result).toEqual({ root: { kind: "text", text: "hello ?" } });
  expect((await h.request("host/unload", { ext: "b" })).result).toBeNull();
  expect((await h.call("b", "tui/render", { surface: { kind: "panel", id: "hello" }, width: 20, height: 2 })).error?.message).toBe("extension not loaded: b");
  expect((await h.render("c", "hello")).result).toBeDefined();
});

test("a bundle error fails only that load", async () => {
  const h = host();
  running = h.child;
  expect(await h.load("broken", "node-import.ts")).toContain('cannot import "node:fs"');
  expect(await h.load("fine", "hello.ts")).toBeUndefined();
  expect((await h.render("fine", "hello")).result).toBeDefined();
});

test("an extension spinning past its deadline answers with VM_STOPPED_ERROR_CODE, then refuses; the others keep answering", async () => {
  const h = host();
  running = h.child;
  await h.load("spinner", "busy.ts");
  await h.load("other", "busy.ts");
  const spinning = h.render("spinner", "spin");
  const other = h.render("other", "fast");
  const answer = await spinning;
  expect(answer.error).toEqual({ code: VM_STOPPED_ERROR_CODE, message: "extension exceeded its 2000 ms limit" });
  expect((await other).result).toEqual({ root: { kind: "text", text: "fast" } });
  expect((await h.render("spinner", "fast")).error?.code).toBe(VM_STOPPED_ERROR_CODE);
  expect((await h.render("other", "fast")).result).toEqual({ root: { kind: "text", text: "fast" } });
});

test("api.fs asks the TUI through the host channel and resolves inside the VM", async () => {
  const asked: string[] = [];
  const h = host(async (method, params) => {
    asked.push(`${method} ${String(params.ext)} ${String(params.path)}`);
    return { text: "body" };
  });
  running = h.child;
  await h.load("reader", "busy.ts", ["fs.read"]);
  expect((await h.render("reader", "read")).result).toEqual({ root: { kind: "text", text: "read:body" } });
  expect(asked).toEqual(["fs/read reader notes.md"]);
});

test("console output is attributed to its extension on stderr", async () => {
  const h = host();
  running = h.child;
  await h.load("talker", "hello.ts");
  h.child.stdin.end();
  expect(await h.child.exited).toBe(0);
  expect(await new Response(h.child.stderr).text()).toContain(JSON.stringify({ ext: "talker", line: "activated" }));
});

describe("trusted extensions (jit: JavaScriptCore on their own thread)", () => {
  test("render like VM extensions, reach api.fs, and log under their id", async () => {
    const h = host(async () => ({ text: "body" }));
    running = h.child;
    expect(await h.load("hello", "hello.ts", [], true)).toBeUndefined();
    expect(await h.load("reader", "busy.ts", ["fs.read"], true)).toBeUndefined();
    expect((await h.render("hello", "hello")).result).toEqual({ root: { kind: "text", text: "hello ?" } });
    expect((await h.render("reader", "read")).result).toEqual({ root: { kind: "text", text: "read:body" } });
    h.child.stdin.end();
    expect(await h.child.exited).toBe(0);
    expect(await new Response(h.child.stderr).text()).toContain(JSON.stringify({ ext: "hello", line: "activated" }));
  });

  test("the realm has no ambient runtime, no way to this thread's Function, and no module loader", async () => {
    const h = host();
    running = h.child;
    for (const jit of [true, false]) {
      await h.load(`escape-${jit}`, "escape.ts", [], jit);
      const answer = await h.render(`escape-${jit}`, "probe");
      const root = answer.result as { root: { text: string } };
      expect(JSON.parse(root.root.text)).toEqual({ ambient: [], escapes: [], dynamicImport: "refused" });
    }
  });

  test("a handler past the deadline stops only that thread; others answer meanwhile", async () => {
    const h = host();
    running = h.child;
    await h.load("spinner", "busy.ts", [], true);
    await h.load("jit-other", "busy.ts", [], true);
    await h.load("vm-other", "busy.ts");
    const spinning = h.render("spinner", "spin");
    const started = performance.now();
    expect((await h.render("jit-other", "fast")).result).toEqual({ root: { kind: "text", text: "fast" } });
    expect((await h.render("vm-other", "fast")).result).toEqual({ root: { kind: "text", text: "fast" } });
    // Separate threads: neither waited for the spinning one.
    expect(performance.now() - started).toBeLessThan(1_000);
    expect((await spinning).error).toEqual({ code: VM_STOPPED_ERROR_CODE, message: "extension exceeded its 2000 ms limit" });
    expect((await h.render("spinner", "fast")).error?.code).toBe(VM_STOPPED_ERROR_CODE);
    expect(await h.load("spinner", "busy.ts", [], true)).toBeUndefined();
    expect((await h.render("spinner", "fast")).result).toEqual({ root: { kind: "text", text: "fast" } });
  });

  test("waiting for api.fs does not count against the deadline", async () => {
    // The TUI answers after more than the 2 s deadline: only execution is limited.
    const h = host(async () => { await Bun.sleep(2_300); return { text: "late" }; });
    running = h.child;
    await h.load("reader", "busy.ts", ["fs.read"], true);
    expect((await h.render("reader", "read")).result).toEqual({ root: { kind: "text", text: "read:late" } });
  }, 10_000);
});
