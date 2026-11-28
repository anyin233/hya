import { beforeAll, expect, test } from "bun:test";
import { join } from "node:path";
import { ExtensionVm, bundleExtension } from "../../../../packages/hya-tui-sdk/src/host";
import type { ExtensionContext, JsonValue, TuiMethod, TuiMethodMap } from "../../../../packages/hya-tui-sdk/src/protocol";
import { isTranscriptNode, type RenderNode } from "../../../../packages/hya-tui-sdk/src/render";

const entry = join(import.meta.dir, "main.ts");
let script = "";
beforeAll(async () => { script = await bundleExtension(entry); });

function flattenText(node: RenderNode | null | undefined): string[] {
  if (!node) return [];
  if (isTranscriptNode(node)) return [`[transcript:${node.sessionId}:follow=${String(node.follow)}]`];
  if (node.kind === "text") return [node.text];
  if (node.kind === "row") return [node.children.map((c) => flattenText(c).join("")).join("")];
  if (node.kind === "column" || node.kind === "box") return node.children.flatMap(flattenText);
  return [];
}

async function open(
  context: ExtensionContext,
  hostHandler?: (method: string, params: Record<string, unknown>) => Promise<unknown>,
) {
  const apiCalls: { method: string; params: Record<string, unknown> }[] = [];
  const vm = await ExtensionVm.create(script, {
    hostCall: async (method, params) => {
      apiCalls.push({ method, params });
      if (hostHandler) return hostHandler(method, params);
      return null;
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
    extension_id: "hya/multi-agent",
    permissions: [
      "tui.panel", "tui.action", "tui.keys", "tui.session.read", "tui.sessions.read",
      "tui.bundle_api", "tui.extension_state.read", "tui.transcript.embed",
    ],
  });
  const activated = await call("tui/activate", { context });
  return { vm, call, apiCalls, contributions: activated.contributions };
}

const sampleContext: ExtensionContext = {
  terminal: { columns: 120, rows: 40 },
  session: { id: "hysec_root", title: "Root", agent: "hya-main", busy: false },
  extension_state: {
    "hya/multi-agent": {
      members: [
        {
          member: "mem_1",
          child: "hysec_child_1",
          subagent_type: "hya-scout",
          description: "Inspect repository",
          depth: 0,
          status: "running",
          summary: "Scanning crates/",
        },
        {
          member: "mem_2",
          child: "hysec_child_2",
          subagent_type: "hya-reviewer",
          description: "Audit diff",
          depth: 0,
          status: "done",
          summary: "Looks good",
        },
      ],
      roster: {
        "hya-scout-1": { handle: "hya-scout-1", session: "hysec_child_1", agent_type: "hya-scout", status: "busy" },
      },
      archived: {
        "hya-old-1": { handle: "hya-old-1", session: "hysec_old_1", agent_type: "hya-task", reason: "completed" },
      },
    } as unknown as JsonValue,
  },
};

test("multi-agent registers subagents pane, subagent-viewer with transcript node, and /subagents command", async () => {
  const { vm, call, apiCalls, contributions } = await open(sampleContext, async (method, params) => {
    if (method === "bundle/api" && params.path === "/members") {
      return {
        session: "hysec_root",
        root: "hysec_root",
        members: (sampleContext.extension_state?.["hya/multi-agent"] as Record<string, unknown>).members,
        roster: (sampleContext.extension_state?.["hya/multi-agent"] as Record<string, unknown>).roster,
        archived: (sampleContext.extension_state?.["hya/multi-agent"] as Record<string, unknown>).archived,
      };
    }
    if (method === "bundle/api" && params.path === "/archive") {
      return { handle: "hya-scout-1", cancelledTurn: false, descendants: ["hysec_nested"] };
    }
    if (method === "bundle/api" && params.path === "/steer") {
      return { from: "user" };
    }
    return null;
  });

  try {
    expect(contributions.panes.map((p) => p.id)).toEqual(["subagents", "subagent-viewer"]);
    expect(contributions.commands.map((c) => c.name)).toEqual(["subagents"]);

    // Render subagents pane (archived hidden by default).
    const pane = await call("tui/render", { surface: { kind: "pane", id: "subagents" }, width: 60, height: 20, now: 0 });
    const text = flattenText(pane.root).join("\n");
    expect(text).toContain("hya-scout-1");
    expect(text).toContain("Audit diff");
    expect(text).not.toContain("hya-old-1");

    // Toggle archived with browser-safe 'a' key.
    await call("tui/key", {
      surface: { kind: "pane", id: "subagents" },
      key: { name: "a", sequence: "a", ctrl: false, shift: false, meta: false },
    });
    const withArchived = await call("tui/render", { surface: { kind: "pane", id: "subagents" }, width: 60, height: 20, now: 0 });
    expect(flattenText(withArchived.root).join("\n")).toContain("hya-old-1");

    // SubagentViewer renders host-virtualized { type: "transcript", sessionId } node.
    const viewer = await call("tui/render", {
      surface: { kind: "pane", id: "subagent-viewer", instance: "hysec_child_1" },
      width: 80,
      height: 20,
      now: 0,
    });
    expect(flattenText(viewer.root).join("\n")).toContain("[transcript:hysec_child_1:follow=true]");

    // /subagents opens both dynamic panes and queries /members.
    const opened = await call("tui/command", { name: "subagents", args: "", raw: "/subagents" });
    expect(opened.commands).toEqual([
      { command: "pane.open", pane: "subagents", slot: "right", focus: true },
      { command: "pane.open", pane: "subagent-viewer", slot: "right", focus: false },
    ]);
    expect(apiCalls.some((c) => c.method === "bundle/api" && c.params.path === "/members")).toBe(true);

    // /subagents steer calls POST /steer with sessionId & text.
    const steered = await call("tui/command", {
      name: "subagents",
      args: "steer hysec_child_1 check tests next",
      raw: "/subagents steer hysec_child_1 check tests next",
    });
    expect(steered.notice).toContain("Steered hysec_child_1");
    expect(apiCalls.some((c) => c.params.path === "/steer" && c.params.sessionId === "hysec_child_1")).toBe(true);

    // /subagents archive calls POST /archive with sessionId.
    const archived = await call("tui/command", {
      name: "subagents",
      args: "archive hysec_child_1",
      raw: "/subagents archive hysec_child_1",
    });
    expect(archived.notice).toContain("Archived hya-scout-1");
    expect(archived.notice).toContain("1 nested subagent");
  } finally {
    vm.dispose();
  }
});
