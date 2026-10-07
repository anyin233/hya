import { expect, test } from "bun:test";
import extension from "../tui/main";
import { TuiExtensionRunner } from "@hya/tui-sdk";
import { renderInspector } from "../tui/panel";

test("pane states distinguish missing integration, loading, errors, and backend identity", () => {
  const states = [
    { kind: "disconnected" as const }, { kind: "loading" as const },
    { kind: "failed" as const, message: "Backend unavailable" },
    { kind: "ready" as const, info: { contractVersion: 1 as const, machine: { hostname: "remote-host", platform: "linux" }, capabilities: { volumes: false as const, scans: false as const, cancel: false as const } } },
  ];
  for (const state of states) {
    const node = renderInspector(state);
    expect(["text", "column"]).toContain(node.kind);
  }
  expect(JSON.stringify(renderInspector(states[0]!))).toContain("not connected");
  expect(JSON.stringify(renderInspector(states[3]!))).toContain("Backend: remote-host");
});

test("the real extension registers an opt-in pane with minimal permissions", async () => {
  const runner = new TuiExtensionRunner(extension);
  const result = await runner.handle({ jsonrpc: "2.0", id: 1, method: "tui/initialize", params: { api_version: 1, sdk_version: "1.0.0", extension_id: "hya-extra/disk-inspector", permissions: ["tui.panel"] } });
  expect(result).toMatchObject({ result: { api_version: 1 } });
  expect(await runner.handle({ jsonrpc: "2.0", id: 2, method: "tui/activate", params: { context: { terminal: { columns: 80, rows: 24 } } } })).toMatchObject({
    result: { contributions: { panels: [{ id: "disk", title: "Disk inspector", placement: "pane" }] } },
  });
  expect(await runner.handle({ jsonrpc: "2.0", id: 3, method: "tui/render", params: { surface: { kind: "panel", id: "disk" }, width: 30, height: 10, now: 0 } })).toMatchObject({
    result: { root: { kind: "column", children: [{ kind: "text", text: "Disk inspector is not connected.", style: { color: "warning" } }, { kind: "text", text: "Waiting for the frontend bundle API bridge.", style: { color: "muted" } }] } },
  });
});
