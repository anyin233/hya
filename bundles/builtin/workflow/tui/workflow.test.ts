import { beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { ExtensionVm, bundleExtension } from "../../../../packages/hya-tui-sdk/src/host";
import type { ExtensionContext, JsonValue, TuiMethod, TuiMethodMap } from "../../../../packages/hya-tui-sdk/src/protocol";
import type { RenderNode } from "../../../../packages/hya-tui-sdk/src/render";

const entry = join(import.meta.dir, "main.ts");
let script = "";
beforeAll(async () => { script = await bundleExtension(entry); });

function flatten(node: RenderNode | null | undefined): string[] {
  if (!node) return [];
  if (node.kind === "text") return [node.text];
  if (node.kind === "progress") return [`[progress:${node.label ?? ""}:${node.value}/${node.total}]`];
  if (node.kind === "row") return [node.children.map((c) => flatten(c).join("")).join("")];
  if (node.kind === "column" || node.kind === "box") return node.children.flatMap(flatten);
  return [];
}

async function open(
  context: ExtensionContext,
  hostHandler: (method: string, params: Record<string, unknown>) => Promise<unknown>,
) {
  const apiCalls: { method: string; params: Record<string, unknown> }[] = [];
  const vm = await ExtensionVm.create(script, {
    hostCall: async (method, params) => {
      apiCalls.push({ method, params });
      return hostHandler(method, params);
    },
  });
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
    extension_id: "hya/workflow",
    permissions: [
      "tui.panel", "tui.action", "tui.keys", "tui.session.read",
      "tui.workspace.read", "tui.bundle_api", "tui.extension_state.read",
    ],
  });
  const activated = await call("tui/activate", { context });
  return { vm, call, apiCalls, contributions: activated.contributions };
}

test("workflow registers workflows pane and /workflow + /workflows commands backed by bundle.api", async () => {
  const wfState = {
    selection: { name: "plan-impl-review" },
    run: {
      id: "wfr_1",
      status: "running",
      stages: [
        { plan: { id: "plan", title: "Plan" }, status: "completed" },
        { plan: { id: "implement", title: "Implement" }, status: "running" },
        { plan: { id: "review", title: "Review" }, status: "pending" },
      ],
    },
  };
  const ctx: ExtensionContext = {
    terminal: { columns: 120, rows: 40 },
    session: { id: "hysec_1", title: "Dev", agent: "hya-main", busy: false },
    workspace: { directory: "/repo" },
    extension_state: { "hya/workflow": wfState as unknown as JsonValue },
  };
  const { vm, call, apiCalls, contributions } = await open(ctx, async (_method, params) => {
    if (params.path === "/workflows") {
      return { workflows: [{ id: "plan-impl-review", name: "plan-impl-review", description: "Plan, implement, and review" }] };
    }
    if (params.path === "/state") return wfState;
    if (params.path === "/command") return { ok: true };
    return null;
  });

  try {
    expect(contributions.panes.map((p) => p.id)).toEqual(["workflows"]);
    expect(contributions.commands.map((c) => c.name).sort()).toEqual(["workflow", "workflows"]);

    // /workflow list refreshes /workflows & /state and opens pane.
    const listed = await call("tui/command", { name: "workflow", args: "list", raw: "/workflow list" });
    expect(listed.notice).toContain("plan-impl-review");
    expect(listed.commands).toEqual([{ command: "pane.open", pane: "workflows", slot: "right", focus: false }]);

    const rendered = await call("tui/render", { surface: { kind: "pane", id: "workflows" }, width: 60, height: 24, now: 0 });
    const text = flatten(rendered.root).join("\n");
    expect(text).toContain("Selected: plan-impl-review");
    expect(text).toContain("[progress:Stages:1/3]");
    expect(text).toContain("✓ Plan (completed)");
    expect(text).toContain("◐ Implement (running)");
    expect(text).toContain("○ Review (pending)");

    // /workflow select and /workflow run hit POST /command.
    await call("tui/command", { name: "workflow", args: "select plan-impl-review", raw: "/workflow select plan-impl-review" });
    await call("tui/command", { name: "workflow", args: "run plan-impl-review", raw: "/workflow run plan-impl-review" });
    const commandCalls = apiCalls.filter((c) => c.params.path === "/command");
    expect(commandCalls.length).toBe(2);
    expect(commandCalls[0]!.params.body).toEqual({ select: { name: "plan-impl-review" } });
    expect(commandCalls[1]!.params.body).toEqual({ run: { name: "plan-impl-review" } });
  } finally {
    vm.dispose();
  }
});
