import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import { TuiExtensionRunner, defineTuiExtension, type RpcEnvelope } from "../src";

let nextId = 0;
const call = async (runner: TuiExtensionRunner, method: string, params: unknown = {}) =>
  await runner.handle({ jsonrpc: "2.0", id: ++nextId, method, params } as RpcEnvelope) as { result?: { readonly [key: string]: unknown } | null; error?: { message: string } };
const context = { terminal: { columns: 120, rows: 40 }, git: { branch: "main", dirty: 2, ahead: 0, behind: 0 } };

describe("extension runner", () => {
  test("reports contributions and renders with the latest context", async () => {
    const runner = new TuiExtensionRunner(defineTuiExtension({
      activate(api) {
        api.registerPanel({ id: "git", title: "Git", replaces: "todos", render: ({ context, width }) => `${context.git?.branch} ${width}` });
        api.registerStatusItem({ id: "branch", label: "Branch", render: ({ context }) => context.git?.branch });
        api.registerRenderer({ id: "compact", target: "tool_call", mode: "replace", priority: 5, render: (input) => input.status === "error" ? null : { kind: "text", text: input.tool } });
        api.registerFormatter({ id: "upper", format: ({ text }) => text.toUpperCase() });
        api.registerInterceptor({ id: "guard", target: "submit", intercept: ({ text }) => text.includes("secret") ? { decision: "block", message: "no secrets" } : undefined });
        api.onContext((next) => next.git?.branch !== "main");
      },
    }));
    expect((await call(runner, "tui/initialize", { api_version: 1, sdk_version: "1.0.0", extension_id: "x", permissions: [] })).result).toEqual({ api_version: 1, sdk_version: "1.0.0" });
    expect((await call(runner, "tui/activate", { context })).result?.contributions).toEqual({
      panels: [{ id: "git", title: "Git", placement: "sidebar", replaces: "todos" }],
      status_items: [{ id: "branch", label: "Branch", priority: 7 }],
      renderers: [{ id: "compact", target: "tool_call", mode: "replace", priority: 5 }],
      formatters: [{ id: "upper", priority: 0 }],
      interceptors: [{ id: "guard", target: "submit", priority: 0 }],
    });
    expect((await call(runner, "tui/render", { surface: { kind: "panel", id: "git" }, width: 30, height: 5 })).result).toEqual({ root: { kind: "text", text: "main 30" } });
    expect((await call(runner, "tui/context", { context: { ...context, git: { ...context.git, branch: "main" } } })).result).toEqual({ invalidate: false });
    expect((await call(runner, "tui/context", { context: { ...context, git: { ...context.git, branch: "dev" } } })).result).toEqual({ invalidate: true });
    expect((await call(runner, "tui/render", { surface: { kind: "status", id: "branch" }, width: 0, height: 0 })).result).toEqual({ root: { kind: "text", text: "dev" } });
    const tool = { id: "t1", tool: "bash", status: "error", summary: "ls" };
    expect((await call(runner, "tui/render", { surface: { kind: "renderer", id: "compact" }, width: 80, height: 0, input: tool })).result).toEqual({ root: null });
    expect((await call(runner, "tui/format", { formatter: "upper", input: { value: null, text: "ok" }, width: 80 })).result).toEqual({ text: "OK" });
    expect((await call(runner, "tui/intercept", { interceptor: "guard", input: { text: "hi" } })).result).toEqual({ decision: "continue" });
    expect((await call(runner, "tui/intercept", { interceptor: "guard", input: { text: "a secret" } })).result).toEqual({ decision: "block", message: "no secrets" });
  });

  test("rejects invalid registrations and late registration", async () => {
    let later: (() => void) | undefined;
    const runner = new TuiExtensionRunner({
      activate(api) {
        expect(() => api.registerRenderer({ id: "c", target: "composer", mode: "replace" as "decorate", render: () => null })).toThrow("only be decorated");
        expect(() => api.registerPanel({ id: "Bad Id", title: "x", render: () => null })).toThrow("invalid panel id");
        api.registerPanel({ id: "p", title: "P", render: () => null });
        expect(() => api.registerPanel({ id: "p", title: "P", render: () => null })).toThrow("duplicate");
        later = () => api.registerStatusItem({ id: "s", label: "S", render: () => "x" });
      },
    });
    await call(runner, "tui/initialize", { api_version: 1 });
    await call(runner, "tui/activate", { context });
    expect(later).toThrow("while activate runs");
  });

  test("requires initialize first and reports handler failures as errors", async () => {
    const runner = new TuiExtensionRunner({ activate: () => { throw new Error("boom"); } });
    expect((await call(runner, "tui/activate", { context })).error?.message).toBe("initialize is required first");
    expect((await call(runner, "tui/initialize", { api_version: 2 })).error?.message).toContain("unsupported api_version");
    await call(runner, "tui/initialize", { api_version: 1 });
    expect((await call(runner, "tui/activate", { context })).error?.message).toBe("boom");
  });
});

test("maps action, key, and command-result handlers and passes render time", async () => {
  const seen: string[] = [];
  const runner = new TuiExtensionRunner(defineTuiExtension({
    activate(api) {
      api.registerPanel({
        id: "events", title: "Events", onKey: (key) => { seen.push(`key:${key.name}`); },
        onResult: (value) => ({ commands: [{ command: "ui.release" }], invalidate: false, ...(value.ok ? { notice: "done" } : { notice: value.error ?? "failed" }) }),
        onAction: (action) => action.name === "string" ? "clicked" : { invalidate: false },
        render: ({ now }) => { seen.push(`now:${now}`); return "ok"; },
      });
    },
  }));
  await call(runner, "tui/initialize", { api_version: 1 });
  const activation = await call(runner, "tui/activate", { context });
  expect(activation.result?.contributions).toEqual({
    panels: [{ id: "events", title: "Events", placement: "sidebar", keys: true }],
    status_items: [], renderers: [], formatters: [], interceptors: [],
  });
  expect((await call(runner, "tui/render", { surface: { kind: "panel", id: "events" }, width: 1, height: 1, now: 123 })).result).toEqual({ root: { kind: "text", text: "ok" } });
  expect(seen).toContain("now:123");
  expect((await call(runner, "tui/action", { surface: { kind: "panel", id: "events" }, action: { name: "string", data: null, button: "left" } })).result).toEqual({ invalidate: true, notice: "clicked" });
  expect((await call(runner, "tui/action", { surface: { kind: "panel", id: "events" }, action: { name: "object", data: null, button: "right" } })).result).toEqual({ invalidate: false });
  expect((await call(runner, "tui/key", { surface: { kind: "panel", id: "events" }, key: { name: "up", sequence: "\\u001b[A", ctrl: false, shift: false, meta: false } })).result).toEqual({ invalidate: true });
  expect(seen).toContain("key:up");
  expect((await call(runner, "tui/command_result", { surface: { kind: "panel", id: "events" }, result: { token: "x", ok: true } })).result).toEqual({ invalidate: false, notice: "done", commands: [{ command: "ui.release" }] });
});
