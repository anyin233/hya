import { beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { ExtensionVm, bundleExtension } from "../../../../packages/hya-tui-sdk/src/host";
import type { ExtensionContext, TuiMethod, TuiMethodMap } from "../../../../packages/hya-tui-sdk/src/protocol";
import type { RenderNode } from "../../../../packages/hya-tui-sdk/src/render";

const entry = join(import.meta.dir, "main.ts");
let script = "";
beforeAll(async () => { script = await bundleExtension(entry); });

function lines(node: RenderNode | null | undefined): string[] {
  if (!node) return [];
  if (node.kind === "text") return [node.text];
  if (node.kind === "row") return [node.children.map((c: RenderNode) => (c.kind === "text" ? c.text : "")).join("")];
  if (node.kind === "column" || node.kind === "box") return node.children.flatMap(lines);
  return [];
}

async function open(context: ExtensionContext) {
  const vm = await ExtensionVm.create(script);
  let id = 0;
  const call = async <M extends TuiMethod>(method: M, params: TuiMethodMap[M]["params"]): Promise<TuiMethodMap[M]["result"]> => {
    id += 1;
    const reply = JSON.parse(await vm.handle(JSON.stringify({ jsonrpc: "2.0", id, method, params }), id)) as {
      result?: TuiMethodMap[M]["result"];
      error?: { message: string };
    };
    if (reply.error || reply.result === undefined) throw new Error(reply.error?.message ?? `${method}: no result`);
    return reply.result;
  };
  await call("tui/initialize", {
    api_version: 2,
    sdk_version: "2.0.0",
    extension_id: "hya/basic-tui-components",
    permissions: [
      "tui.panel", "tui.action", "tui.keys", "tui.session.read", "tui.sessions.read",
      "tui.projects.read", "tui.todos.read", "tui.status.read", "tui.session.control", "tui.project.control",
    ],
  });
  const activated = await call("tui/activate", { context });
  return { vm, call, contributions: activated.contributions };
}

test("basic-tui-components registers jobs panel replacing built-in jobs and renders live activity", async () => {
  const ctx: ExtensionContext = {
    terminal: { columns: 100, rows: 30 },
    session: { id: "s1", title: "Main Work", agent: "hya-main", busy: true },
    sessions: {
      ready: true,
      selected: "s1",
      items: [
        { id: "s1", title: "Main Work", agent: "hya-main", temporary: false, archived: false, busy: true, waiting: false, members: [{ child: "s1_c", handle: "hya-scout-1" }] },
        { id: "s1_c", agent: "hya-scout", parent: "s1", temporary: false, archived: false, busy: true, waiting: false },
        { id: "s2", title: "Background", agent: "hya-main", temporary: false, archived: false, busy: true, waiting: false },
        { id: "s3", title: "Approval Needed", agent: "hya-main", temporary: false, archived: false, busy: false, waiting: true },
      ],
    },
  };
  const { vm, call, contributions } = await open(ctx);
  try {
    const jobsPanel = contributions.panels.find((p) => p.id === "jobs");
    expect(jobsPanel).toBeDefined();
    expect(jobsPanel?.replaces).toBe("jobs");
    expect(jobsPanel?.placement).toBe("pane");

    const rendered = await call("tui/render", { surface: { kind: "panel", id: "jobs" }, width: 60, height: 15, now: 0 });
    expect(lines(rendered.root)).toEqual([
      "● Main Work · turn running",
      "↳ hya-scout-1 · running",
      "● Background · busy",
      "◌ Approval Needed · waiting for input",
    ]);

    const clicked = await call("tui/action", {
      surface: { kind: "panel", id: "jobs" },
      action: { name: "open", data: { id: "s2" }, button: "left" },
    });
    expect(clicked.commands).toEqual([{ command: "session.open", id: "s2", token: "open" }]);

    await call("tui/context", {
      context: {
        terminal: { columns: 100, rows: 30 },
        session: { id: "s1", title: "Main Work", agent: "hya-main", busy: false },
        sessions: { ready: true, selected: "s1", items: [] },
      },
    });
    const idle = await call("tui/render", { surface: { kind: "panel", id: "jobs" }, width: 60, height: 15, now: 0 });
    expect(lines(idle.root)[0]).toContain("No active jobs");
  } finally {
    vm.dispose();
  }
});
