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
  if (node.kind === "row") return [node.children.map((c) => flatten(c).join("")).join("")];
  if (node.kind === "column") return node.children.flatMap(flatten);
  if (node.kind === "box") return [...(node.title ? [node.title] : []), ...node.children.flatMap(flatten)];
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
    extension_id: "hya/plan",
    permissions: ["tui.render", "tui.status_item", "tui.session.read", "tui.transcript.read"],
  });
  const activated = await call("tui/activate", { context });
  return { vm, call, contributions: activated.contributions };
}

test("plan renders plan_exit tool cards, ignores unrelated tools, and shows Plan status badge on hya-plan", async () => {
  const ctx: ExtensionContext = {
    terminal: { columns: 100, rows: 30 },
    session: { id: "s1", agent: "hya-plan", busy: false },
  };
  const { vm, call, contributions } = await open(ctx);
  try {
    expect(contributions.renderers.map((r) => r.id)).toEqual(["plan-exit-card"]);
    expect(contributions.status_items.map((s) => s.id)).toEqual(["plan-mode"]);

    const badge = await call("tui/render", { surface: { kind: "status", id: "plan-mode" }, width: 40, height: 1, now: 0 });
    expect(flatten(badge.root)).toEqual(["PLAN (read-only)"]);

    const approved = await call("tui/render", {
      surface: { kind: "renderer", id: "plan-exit-card" },
      width: 60,
      height: 0,
      now: 0,
      input: {
        id: "call_1",
        tool: "plan_exit",
        status: "completed",
        summary: "Plan exit",
        output: "User approved switching to build agent.",
      } as unknown as JsonValue,
    });
    expect(approved.card?.summary).toBe("Plan Mode Exit");
    expect(approved.card?.statusChip).toBe("approved · switching to build agent");

    const rejected = await call("tui/render", {
      surface: { kind: "renderer", id: "plan-exit-card" },
      width: 60,
      height: 0,
      now: 0,
      input: {
        id: "call_2",
        tool: "plan_exit",
        status: "error",
        summary: "Plan exit",
        error: "plan exit rejected by user",
      } as unknown as JsonValue,
    });
    expect(rejected.card?.statusChip).toBe("rejected");
    expect(rejected.root).toBeNull();

    // Unrelated tools fall back cleanly to built-in host rendering.
    const other = await call("tui/render", {
      surface: { kind: "renderer", id: "plan-exit-card" },
      width: 60,
      height: 0,
      now: 0,
      input: { id: "call_3", tool: "bash", status: "completed", summary: "ls" } as unknown as JsonValue,
    });
    expect(other.root).toBeNull();

    // Switching away from hya-plan hides the Plan badge.
    await call("tui/context", {
      context: { terminal: { columns: 100, rows: 30 }, session: { id: "s1", agent: "hya-main", busy: false } },
    });
    const noBadge = await call("tui/render", { surface: { kind: "status", id: "plan-mode" }, width: 40, height: 1, now: 0 });
    expect(noBadge.root).toBeNull();
  } finally {
    vm.dispose();
  }
});
