/**
 * `hya/multi-agent#subagent-viewer` dynamic pane: displays a host-virtualized
 * streaming transcript (`{ type: "transcript", sessionId }`) for a pinned or
 * currently selected subagent without cloning transcript trees over IPC.
 */
import { transcriptNode, type ExtensionContext, type HandlerResult, type KeyEvent, type RenderNode, type TuiExtensionApi } from "@hya/tui-sdk";
import { buildSubagentRows, resolveSelectedRow, truncate, type MultiAgentUiState } from "./state";

function targetFromParams(params: unknown): string | undefined {
  if (typeof params === "object" && params !== null && "sessionId" in params) {
    const id = (params as Record<string, unknown>).sessionId;
    if (typeof id === "string" && id.trim()) return id.trim();
  }
  return undefined;
}

export function registerSubagentViewer(api: TuiExtensionApi, ui: MultiAgentUiState): void {
  const render = (
    context: ExtensionContext,
    width: number,
    instance?: string,
    params?: unknown,
  ): RenderNode => {
    const rows = buildSubagentRows(context, api.extensionState.get(), ui);
    const pinnedId = instance || targetFromParams(params);
    const selected = resolveSelectedRow(rows, ui);
    const sessionId = pinnedId ?? selected?.sessionId;

    if (!sessionId) {
      return {
        kind: "text",
        text: "Select a subagent to preview its transcript.",
        style: { color: "muted" },
      };
    }

    const row = rows.find((candidate) => candidate.sessionId === sessionId);
    const label = row?.label ?? sessionId;
    const status = row?.status ?? "active";
    const modeTag = pinnedId ? "pinned" : "following";
    const followTag = ui.follow ? "follow" : "paused";

    return {
      kind: "column",
      children: [
        {
          kind: "row",
          children: [
            {
              kind: "text",
              text: truncate(`${label} · ${status}`, Math.max(8, width - 22)),
              style: { color: "heading", bold: true },
            },
            {
              kind: "text",
              text: `  [${modeTag} · ${followTag}]`,
              style: { color: "muted" },
            },
          ],
        },
        transcriptNode(sessionId, {
          follow: ui.follow,
          compact: width < 60,
        }),
      ],
    };
  };

  const onKey = (key: KeyEvent, context: ExtensionContext, meta?: { readonly instance?: string }): HandlerResult => {
    if (key.name === "escape") {
      return { invalidate: false, commands: [{ command: "ui.release" }] };
    }
    if (key.ctrl || key.meta) return { invalidate: false };
    if (key.sequence === "f") {
      ui.follow = !ui.follow;
      return { invalidate: true };
    }
    if (key.sequence === "p") {
      const rows = buildSubagentRows(context, api.extensionState.get(), ui);
      const targetId = meta?.instance ?? resolveSelectedRow(rows, ui)?.sessionId;
      if (!targetId) return { invalidate: false };
      const pinned = !meta?.instance;
      return {
        invalidate: true,
        commands: [
          {
            command: "pane.pin",
            pane: "subagent-viewer",
            ...(pinned ? { instance: targetId } : {}),
            pinned,
          },
        ],
      };
    }
    if (key.sequence === "q") {
      return {
        invalidate: false,
        commands: [
          {
            command: "pane.close",
            pane: "subagent-viewer",
            ...(meta?.instance ? { instance: meta.instance } : {}),
          },
        ],
      };
    }
    return { invalidate: false };
  };

  api.registerPane({
    id: "subagent-viewer",
    title: "Subagent Viewer",
    defaultSlot: "right",
    defaultSize: { width: "45%", minWidth: 28 },
    pinnable: true,
    closable: true,
    render: ({ context, width, instance, params }) => render(context, width, instance, params),
    onKey,
  });
}
