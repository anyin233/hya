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
  if (node.kind === "column" || node.kind === "box") return node.children.flatMap(flatten);
  return [];
}

async function open(
  context: ExtensionContext,
  hostHandler?: (method: string, params: Record<string, unknown>) => Promise<unknown>,
) {
  const vm = await ExtensionVm.create(script, { hostCall: hostHandler });
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
    extension_id: "hya/agent-channels",
    permissions: [
      "tui.render", "tui.status_item", "tui.session.read",
      "tui.transcript.read", "tui.bundle_api", "tui.extension_state.read",
    ],
  });
  const activated = await call("tui/activate", { context });
  return { vm, call, contributions: activated.contributions };
}

test("agent-channels renders send/list_channel/report cards and unread mail status item", async () => {
  const ctx: ExtensionContext = {
    terminal: { columns: 100, rows: 30 },
    session: { id: "s1", agent: "hya-main", busy: false },
    extension_state: {
      "hya/agent-channels": {
        inboxes: { lead: [0, 1, 2] },
        cursors: { lead: 1 },
      } as unknown as JsonValue,
    },
  };
  const { vm, call, contributions } = await open(ctx, async (method, params) => {
    if (method === "bundle/api" && params.path === "/inbox") {
      return { handle: "lead", cursor: 0, total: 1, unread: [{ from: "scout", channel: "dm", body: "ping" }] };
    }
    return null;
  });

  try {
    expect(contributions.renderers.map((r) => r.id)).toEqual(["channel-tools"]);
    expect(contributions.status_items.map((s) => s.id)).toEqual(["unread-mail"]);

    // 2 unread from projection (indices 1 and 2 >= cursor 1).
    const status = await call("tui/render", { surface: { kind: "status", id: "unread-mail" }, width: 30, height: 1, now: 0 });
    expect(flatten(status.root)).toEqual(["2 unread"]);

    // When projection has 0 unread, falls back to bundle.api /inbox.
    await call("tui/context", {
      context: {
        terminal: { columns: 100, rows: 30 },
        session: { id: "s1", agent: "hya-main", busy: false },
        extension_state: { "hya/agent-channels": { inboxes: { lead: [0] }, cursors: { lead: 1 } } as unknown as JsonValue },
      },
    });
    const fallbackStatus = await call("tui/render", { surface: { kind: "status", id: "unread-mail" }, width: 30, height: 1, now: 0 });
    expect(flatten(fallbackStatus.root)).toEqual(["1 unread"]);

    // send card
    const sendCard = await call("tui/render", {
      surface: { kind: "renderer", id: "channel-tools" },
      width: 60,
      height: 0,
      now: 0,
      input: {
        id: "t1",
        tool: "send",
        status: "completed",
        summary: "sent",
        input: { channel: "#unit-default", body: "Start review pass" },
        output: "Delivered to 2 peers",
      } as unknown as JsonValue,
    });
    expect(flatten(sendCard.root).join("\n")).toContain("send → #unit-default");
    expect(flatten(sendCard.root).join("\n")).toContain("Start review pass");

    // list_channel card
    const listCard = await call("tui/render", {
      surface: { kind: "renderer", id: "channel-tools" },
      width: 60,
      height: 0,
      now: 0,
      input: {
        id: "t2",
        tool: "list_channel",
        status: "completed",
        summary: "channels",
        output: "#unit-default (unread: 2)\nDM-scout (unread: 0)",
      } as unknown as JsonValue,
    });
    expect(flatten(listCard.root).join("\n")).toContain("channels · snapshot");
    expect(flatten(listCard.root).join("\n")).toContain("#unit-default (unread: 2)");

    // report card
    const reportCard = await call("tui/render", {
      surface: { kind: "renderer", id: "channel-tools" },
      width: 60,
      height: 0,
      now: 0,
      input: {
        id: "t3",
        tool: "report",
        status: "completed",
        summary: "done",
        input: { outcome: "done", result: "All checks green" },
      } as unknown as JsonValue,
    });
    expect(flatten(reportCard.root).join("\n")).toContain("report · done");
    expect(flatten(reportCard.root).join("\n")).toContain("All checks green");

    // Non-channel tools fall back cleanly.
    const other = await call("tui/render", {
      surface: { kind: "renderer", id: "channel-tools" },
      width: 60,
      height: 0,
      now: 0,
      input: { id: "t4", tool: "read", status: "completed", summary: "read" } as unknown as JsonValue,
    });
    expect(other.root).toBeNull();
  } finally {
    vm.dispose();
  }
});
