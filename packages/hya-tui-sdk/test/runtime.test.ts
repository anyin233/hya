import { describe, expect, test } from "bun:test";
import { TuiExtensionRunner, defineTuiExtension, transcriptNode, type RpcEnvelope } from "../src";

let nextId = 0;
const call = async (runner: TuiExtensionRunner, method: string, params: unknown = {}) =>
  await runner.handle({ jsonrpc: "2.0", id: ++nextId, method, params } as RpcEnvelope) as { result?: { readonly [key: string]: unknown } | null; error?: { message: string } };
const context = { terminal: { columns: 120, rows: 40 }, git: { branch: "main", dirty: 2, ahead: 0, behind: 0 } };

describe("extension runner v2", () => {
  test("reports v2 contributions (including panes and commands) and renders with the latest context", async () => {
    const runner = new TuiExtensionRunner(defineTuiExtension({
      activate(api) {
        api.registerPanel({ id: "git", title: "Git", replaces: "todos", render: ({ context, width }) => `${context.git?.branch} ${width}` });
        api.registerPane({
          id: "viewer",
          title: "Subagent",
          defaultSlot: "right",
          defaultSize: { width: "40%", minWidth: 32 },
          pinnable: true,
          closable: true,
          render: ({ instance, params }) => ({
            kind: "column",
            children: [
              { kind: "text", text: `pane:${instance ?? "none"}:${JSON.stringify(params ?? null)}` },
              transcriptNode(instance ?? "hysec_default", { follow: true }),
            ],
          }),
        });
        api.registerCommand({
          name: "subagents",
          description: "Open the subagents pane",
          argumentHint: "[filter]",
          execute: ({ args }) => ({
            notice: `opening ${args || "all"}`,
            commands: [{ command: "pane.open", pane: "viewer", instance: "hysec_child_1", slot: "right", params: { filter: args } }],
          }),
        });
        api.registerStatusItem({ id: "branch", label: "Branch", render: ({ context }) => context.git?.branch });
        api.registerRenderer({ id: "compact", target: "tool_call", mode: "replace", priority: 5, render: (input) => input.status === "error" ? null : { kind: "text", text: input.tool } });
        api.registerFormatter({ id: "upper", format: ({ text }) => text.toUpperCase() });
        api.registerInterceptor({ id: "guard", target: "submit", intercept: ({ text }) => text.includes("secret") ? { decision: "block", message: "no secrets" } : undefined });
        api.onContext((next) => next.git?.branch !== "main");
      },
    }));
    expect((await call(runner, "tui/initialize", { api_version: 2, sdk_version: "2.0.0", extension_id: "hya/multi-agent", permissions: [] })).result).toEqual({ api_version: 2, sdk_version: "2.0.0" });
    expect((await call(runner, "tui/activate", { context })).result?.contributions).toEqual({
      panels: [{ id: "git", title: "Git", placement: "sidebar", replaces: "todos" }],
      panes: [{ id: "viewer", title: "Subagent", default_slot: "right", default_size: { width: "40%", min_width: 32 }, pinnable: true, closable: true }],
      commands: [{ name: "subagents", description: "Open the subagents pane", argument_hint: "[filter]" }],
      status_items: [{ id: "branch", label: "Branch", priority: 7 }],
      renderers: [{ id: "compact", target: "tool_call", mode: "replace", priority: 5 }],
      formatters: [{ id: "upper", priority: 0 }],
      interceptors: [{ id: "guard", target: "submit", priority: 0 }],
    });
    expect((await call(runner, "tui/render", { surface: { kind: "panel", id: "git" }, width: 30, height: 5 })).result).toEqual({ root: { kind: "text", text: "main 30" } });
    expect((await call(runner, "tui/render", { surface: { kind: "pane", id: "viewer", instance: "hysec_c1" }, width: 50, height: 20, input: { filter: "busy" } })).result).toEqual({
      root: {
        kind: "column",
        children: [
          { kind: "text", text: 'pane:hysec_c1:{"filter":"busy"}' },
          { type: "transcript", sessionId: "hysec_c1", follow: true },
        ],
      },
    });
    expect((await call(runner, "tui/command", { name: "subagents", args: "busy", raw: "/subagents busy" })).result).toEqual({
      invalidate: true,
      notice: "opening busy",
      commands: [{ command: "pane.open", pane: "viewer", instance: "hysec_child_1", slot: "right", params: { filter: "busy" } }],
    });
    expect((await call(runner, "tui/context", { context: { ...context, git: { ...context.git, branch: "main" } } })).result).toEqual({ invalidate: false });
    expect((await call(runner, "tui/context", { context: { ...context, git: { ...context.git, branch: "dev" } } })).result).toEqual({ invalidate: true });
    expect((await call(runner, "tui/render", { surface: { kind: "status", id: "branch" }, width: 0, height: 0 })).result).toEqual({ root: { kind: "text", text: "dev" } });
    const tool = { id: "t1", tool: "bash", status: "error", summary: "ls" };
    expect((await call(runner, "tui/render", { surface: { kind: "renderer", id: "compact" }, width: 80, height: 0, input: tool })).result).toEqual({ root: null });
    expect((await call(runner, "tui/format", { formatter: "upper", input: { value: null, text: "ok" }, width: 80 })).result).toEqual({ text: "OK" });
    expect((await call(runner, "tui/intercept", { interceptor: "guard", input: { text: "hi" } })).result).toEqual({ decision: "continue" });
    expect((await call(runner, "tui/intercept", { interceptor: "guard", input: { text: "a secret" } })).result).toEqual({ decision: "block", message: "no secrets" });
  });

  test("rejects invalid registrations (panels, panes, commands) and late registration", async () => {
    let later: (() => void) | undefined;
    const runner = new TuiExtensionRunner({
      activate(api) {
        expect(() => api.registerRenderer({ id: "c", target: "composer", mode: "replace" as "decorate", render: () => null })).toThrow("only be decorated");
        expect(() => api.registerPanel({ id: "Bad Id", title: "x", render: () => null })).toThrow("invalid panel id");
        expect(() => api.registerPane({ id: "p1", title: "P", defaultSlot: "bogus" as "right", render: () => null })).toThrow("invalid defaultSlot");
        expect(() => api.registerPane({ id: "p2", title: "P", defaultSize: { width: "120%" }, render: () => null })).toThrow("percentage string");
        expect(() => api.registerCommand({ name: "skill:hijack", description: "bad", execute: () => undefined })).toThrow("skill: prefix is reserved");
        expect(() => api.registerCommand({ name: "Bad Cmd", description: "bad", execute: () => undefined })).toThrow("invalid command name");
        expect(() => api.registerCommand({ name: "empty", description: "   ", execute: () => undefined })).toThrow("description must be 1-160");
        api.registerPanel({ id: "p", title: "P", render: () => null });
        expect(() => api.registerPanel({ id: "p", title: "P", render: () => null })).toThrow("duplicate");
        api.registerCommand({ name: "ok", description: "ok", execute: () => undefined });
        expect(() => api.registerCommand({ name: "ok", description: "dup", execute: () => undefined })).toThrow("duplicate command");
        later = () => api.registerStatusItem({ id: "s", label: "S", render: () => "x" });
      },
    });
    await call(runner, "tui/initialize", { api_version: 2, extension_id: "acme/test", permissions: [] });
    await call(runner, "tui/activate", { context });
    expect(later).toThrow("while activate runs");
  });

  test("requires initialize first, rejects v1 api_version, and reports handler failures as errors", async () => {
    const runner = new TuiExtensionRunner({ activate: () => { throw new Error("boom"); } });
    expect((await call(runner, "tui/activate", { context })).error?.message).toBe("initialize is required first");
    expect((await call(runner, "tui/initialize", { api_version: 1 })).error?.message).toContain("unsupported api_version");
    expect((await call(runner, "tui/initialize", { api_version: 3 })).error?.message).toContain("unsupported api_version");
    await call(runner, "tui/initialize", { api_version: 2 });
    expect((await call(runner, "tui/activate", { context })).error?.message).toBe("boom");
  });

  test("enforces permissions and bundle-ID confinement on api.bundleApi and api.extensionState", async () => {
    const calls: { method: string; params: Record<string, unknown> }[] = [];
    const runner = new TuiExtensionRunner(defineTuiExtension({
      activate(api) {
        api.registerPanel({
          id: "probe",
          title: "Probe",
          render: async () => {
            const ownState = api.extensionState.get<{ count: number }>();
            const remoteState = await api.extensionState.read<{ count: number }>({ sessionId: "hysec_sub" });
            const apiReply = await api.bundleApi.post<{ ok: boolean }>("/runs", { step: 1 }, { query: { dry: "true" } });
            let crossApi = "allowed";
            try { await api.bundleApi.get("/secret", { bundleId: "hya/other" }); } catch (err) { crossApi = (err as Error).message; }
            let crossState = "allowed";
            try { api.extensionState.get("hya/other"); } catch (err) { crossState = (err as Error).message; }
            let traversal = "allowed";
            try { await api.bundleApi.get("/../other/api/secret"); } catch (err) { traversal = (err as Error).message; }
            return JSON.stringify({ bundleId: api.bundleId, ownState, remoteState, apiReply, crossApi, crossState, traversal });
          },
        });
      },
    }), async (method, params) => {
      calls.push({ method, params });
      if (method === "bundle/api") return { ok: true };
      if (method === "extension_state/read") return { state: { count: 9 } };
      return null;
    });

    await call(runner, "tui/initialize", {
      api_version: 2,
      sdk_version: "2.0.0",
      extension_id: "hya/workflow",
      permissions: ["tui.panel", "tui.bundle_api", "tui.extension_state.read"],
    });
    await call(runner, "tui/activate", {
      context: { ...context, extension_state: { "hya/workflow": { count: 3 } } },
    });

    const rendered = await call(runner, "tui/render", { surface: { kind: "panel", id: "probe" }, width: 80, height: 10 });
    const parsed = JSON.parse((rendered.result?.root as { text: string }).text);
    expect(parsed.bundleId).toBe("hya/workflow");
    expect(parsed.ownState).toEqual({ count: 3 });
    expect(parsed.remoteState).toEqual({ count: 9 });
    expect(parsed.apiReply).toEqual({ ok: true });
    expect(parsed.crossApi).toContain("confined to bundle hya/workflow");
    expect(parsed.crossState).toContain("confined to bundle hya/workflow");
    expect(parsed.traversal).toContain("traversal");
    expect(calls).toEqual([
      { method: "extension_state/read", params: { bundleId: "hya/workflow", sessionId: "hysec_sub" } },
      { method: "bundle/api", params: { method: "POST", path: "/runs", bundleId: "hya/workflow", body: { step: 1 }, query: { dry: "true" } } },
    ]);
  });

  test("rejects api.bundleApi and api.extensionState when permissions are absent", async () => {
    const runner = new TuiExtensionRunner(defineTuiExtension({
      activate(api) {
        api.registerPanel({
          id: "denied",
          title: "Denied",
          render: async () => {
            const out: Record<string, string> = {};
            try { await api.bundleApi.get("/runs"); } catch (err) { out.api = (err as Error).message; }
            try { api.extensionState.get(); } catch (err) { out.stateGet = (err as Error).message; }
            try { await api.extensionState.read(); } catch (err) { out.stateRead = (err as Error).message; }
            return JSON.stringify(out);
          },
        });
      },
    }), async () => { throw new Error("should not reach host"); });

    await call(runner, "tui/initialize", { api_version: 2, sdk_version: "2.0.0", extension_id: "hya/workflow", permissions: ["tui.panel"] });
    await call(runner, "tui/activate", { context });
    const rendered = await call(runner, "tui/render", { surface: { kind: "panel", id: "denied" }, width: 80, height: 10 });
    expect(JSON.parse((rendered.result?.root as { text: string }).text)).toEqual({
      api: "missing permission tui.bundle_api",
      stateGet: "missing permission tui.extension_state.read",
      stateRead: "missing permission tui.extension_state.read",
    });
  });

  test("allows explicitly authorized cross-bundle reads and API calls when initialized with authorized_bundles", async () => {
    const calls: Record<string, unknown>[] = [];
    const runner = new TuiExtensionRunner(defineTuiExtension({
      activate(api) {
        api.registerPanel({
          id: "auth",
          title: "Auth",
          render: async () => {
            const state = api.extensionState.get("hya/multi-agent");
            const reply = await api.bundleApi.get("/agents", { bundleId: "hya/multi-agent" });
            return JSON.stringify({ state, reply });
          },
        });
      },
    }), async (_method, params) => {
      calls.push(params);
      return { agents: 2 };
    });

    await call(runner, "tui/initialize", {
      api_version: 2,
      sdk_version: "2.0.0",
      extension_id: "hya/dashboard",
      permissions: ["tui.panel", "tui.bundle_api", "tui.extension_state.read"],
      authorized_bundles: ["hya/multi-agent"],
    });
    await call(runner, "tui/activate", {
      context: { ...context, extension_state: { "hya/multi-agent": { active: 2 } } },
    });
    const rendered = await call(runner, "tui/render", { surface: { kind: "panel", id: "auth" }, width: 80, height: 10 });
    expect(JSON.parse((rendered.result?.root as { text: string }).text)).toEqual({
      state: { active: 2 },
      reply: { agents: 2 },
    });
    expect(calls).toEqual([{ method: "GET", path: "/agents", bundleId: "hya/multi-agent" }]);
  });
});

test("maps action, key, and command-result handlers for panels, panes, and commands", async () => {
  const seen: string[] = [];
  const runner = new TuiExtensionRunner(defineTuiExtension({
    activate(api) {
      api.registerPanel({
        id: "events", title: "Events", onKey: (key) => { seen.push(`key:${key.name}`); },
        onResult: (value) => ({ commands: [{ command: "ui.release" }], invalidate: false, ...(value.ok ? { notice: "done" } : { notice: value.error ?? "failed" }) }),
        onAction: (action) => action.name === "string" ? "clicked" : { invalidate: false },
        render: ({ now }) => { seen.push(`now:${now}`); return "ok"; },
      });
      api.registerPane({
        id: "live", title: "Live",
        onKey: (key, _ctx, meta) => { seen.push(`pane-key:${meta?.instance}:${key.name}`); return { commands: [{ command: "pane.pin", pane: "live", ...(meta?.instance ? { instance: meta.instance } : {}), pinned: true }] }; },
        onAction: (action, _ctx, meta) => { seen.push(`pane-action:${meta?.instance}:${action.name}`); return "pane-clicked"; },
        onResult: (value, _ctx, meta) => { seen.push(`pane-result:${meta?.instance}:${value.token}`); return { invalidate: false }; },
        render: () => "live",
      });
      api.registerCommand({
        name: "ping",
        description: "Ping backend",
        execute: () => ({ commands: [{ command: "bundle.api", method: "GET", path: "/ping", token: "t-ping" }] }),
        onResult: (res) => `ping:${res.ok}`,
      });
    },
  }));
  await call(runner, "tui/initialize", { api_version: 2, sdk_version: "2.0.0", extension_id: "acme/events", permissions: [] });
  const activation = await call(runner, "tui/activate", { context });
  expect(activation.result?.contributions).toEqual({
    panels: [{ id: "events", title: "Events", placement: "sidebar", keys: true }],
    panes: [{ id: "live", title: "Live", keys: true }],
    commands: [{ name: "ping", description: "Ping backend" }],
    status_items: [], renderers: [], formatters: [], interceptors: [],
  });
  expect((await call(runner, "tui/render", { surface: { kind: "panel", id: "events" }, width: 1, height: 1, now: 123 })).result).toEqual({ root: { kind: "text", text: "ok" } });
  expect(seen).toContain("now:123");
  expect((await call(runner, "tui/action", { surface: { kind: "panel", id: "events" }, action: { name: "string", data: null, button: "left" } })).result).toEqual({ invalidate: true, notice: "clicked" });
  expect((await call(runner, "tui/action", { surface: { kind: "panel", id: "events" }, action: { name: "object", data: null, button: "right" } })).result).toEqual({ invalidate: false });
  expect((await call(runner, "tui/key", { surface: { kind: "panel", id: "events" }, key: { name: "up", sequence: "\u001b[A", ctrl: false, shift: false, meta: false } })).result).toEqual({ invalidate: true });
  expect(seen).toContain("key:up");
  expect((await call(runner, "tui/command_result", { surface: { kind: "panel", id: "events" }, result: { token: "x", ok: true } })).result).toEqual({ invalidate: false, notice: "done", commands: [{ command: "ui.release" }] });

  expect((await call(runner, "tui/key", { surface: { kind: "pane", id: "live", instance: "i1" }, key: { name: "p", sequence: "p", ctrl: false, shift: false, meta: false } })).result).toEqual({
    invalidate: true,
    commands: [{ command: "pane.pin", pane: "live", instance: "i1", pinned: true }],
  });
  expect((await call(runner, "tui/action", { surface: { kind: "pane", id: "live", instance: "i1" }, action: { name: "select", data: null, button: "left" } })).result).toEqual({ invalidate: true, notice: "pane-clicked" });
  expect((await call(runner, "tui/command_result", { surface: { kind: "pane", id: "live", instance: "i1" }, result: { token: "tok1", ok: true } })).result).toEqual({ invalidate: false });
  expect((await call(runner, "tui/command_result", { surface: { kind: "command", id: "ping" }, result: { token: "t-ping", ok: true } })).result).toEqual({ invalidate: true, notice: "ping:true" });
  expect(seen).toContain("pane-key:i1:p");
  expect(seen).toContain("pane-action:i1:select");
  expect(seen).toContain("pane-result:i1:tok1");
});
