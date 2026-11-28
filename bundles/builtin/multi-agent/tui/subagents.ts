/**
 * `hya/multi-agent#subagents` dynamic pane: lists active (and optionally archived)
 * subagents for the open session with browser-safe keyboard controls for selecting,
 * pinning/opening viewers, archiving, and refreshing via `bundle.api`.
 */
import type { ExtensionContext, HandlerResult, KeyEvent, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";
import { buildSubagentRows, resolveSelectedRow, truncate, type MultiAgentSnapshot, type MultiAgentUiState, type SubagentRow } from "./state";

const isEnter = (key: KeyEvent): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter");

function statusColor(row: SubagentRow): string {
  if (row.archived) return "muted";
  switch (row.status) {
    case "running":
    case "starting":
      return "activity";
    case "done":
      return "success";
    case "failed":
      return "error";
    case "waiting":
    case "cancelled":
      return "warning";
    default:
      return "muted";
  }
}

export function registerSubagentsPane(api: TuiExtensionApi, ui: MultiAgentUiState): void {
  const render = (context: ExtensionContext, width: number): RenderNode => {
    const state = api.extensionState.get();
    const rows = buildSubagentRows(context, state, ui);
    const selected = resolveSelectedRow(rows, ui);
    if (selected && ui.selectedSessionId !== selected.sessionId) {
      ui.selectedSessionId = selected.sessionId;
    }

    const children: RenderNode[] = [];
    const header = ui.showArchived ? "Subagents · archived included" : "Subagents";
    children.push({
      kind: "text",
      text: truncate(`${header} (${rows.length})`, width),
      style: { color: "heading", bold: true },
    });

    if (!rows.length) {
      children.push({
        kind: "text",
        text: ui.showArchived
          ? "No subagents in this conversation."
          : "No active subagents in this conversation.",
        style: { color: "muted" },
      });
    } else {
      for (const row of rows) {
        const active = selected?.sessionId === row.sessionId;
        const prefix = `${active ? "▸" : " "}${"  ".repeat(row.depth)}`;
        const action = { name: "select", data: { sessionId: row.sessionId } };
        children.push({
          kind: "row",
          children: [
            {
              kind: "text",
              text: truncate(`${prefix}${row.label} `, Math.max(4, width - 14)),
              style: { color: active ? "focus" : "fg", ...(active ? { bold: true } : {}) },
              action,
            },
            {
              kind: "text",
              text: `· ${row.status}`,
              style: { color: statusColor(row) },
              action,
            },
          ],
        });
        const detail = row.archived
          ? `${row.handle || row.sessionId}${row.archiveReason ? ` · ${row.archiveReason}` : ""}`
          : row.summary || row.sessionId;
        children.push({
          kind: "text",
          text: truncate(`  ${"  ".repeat(row.depth)}${detail}`, width),
          style: { color: "muted" },
          action,
        });
      }
    }

    const confirming = ui.confirmArchiveSessionId
      ? rows.find((row) => row.sessionId === ui.confirmArchiveSessionId)
      : undefined;
    if (confirming) {
      children.push({
        kind: "text",
        text: truncate(`Archive ${confirming.label}? Enter confirms · any other key cancels`, width),
        style: { color: "warning" },
      });
    } else {
      children.push({
        kind: "text",
        text: truncate(
          `↑↓ select · Enter pin · v view · d archive · a ${ui.showArchived ? "hide" : "show"} archived · r refresh · q close`,
          width,
        ),
        style: { color: "muted" },
      });
    }

    return { kind: "column", children };
  };

  const onKey = async (key: KeyEvent, context: ExtensionContext): Promise<HandlerResult> => {
    const rows = buildSubagentRows(context, api.extensionState.get(), ui);
    const selected = resolveSelectedRow(rows, ui);

    if (ui.confirmArchiveSessionId) {
      const targetId = ui.confirmArchiveSessionId;
      ui.confirmArchiveSessionId = undefined;
      const target = rows.find((row) => row.sessionId === targetId);
      if (target && isEnter(key) && !key.ctrl && !key.shift) {
        try {
          const receipt = await api.bundleApi.post<{
            handle?: string;
            descendants?: string[];
          }>("/archive", {}, { sessionId: targetId });
          if (context.session?.id) {
            ui.apiSnapshot = await api.bundleApi
              .get<MultiAgentSnapshot>("/members", { sessionId: context.session.id })
              .catch(() => ui.apiSnapshot);
          }
          const nested = Array.isArray(receipt?.descendants) ? receipt.descendants.length : 0;
          return {
            invalidate: true,
            notice: `Archived ${receipt?.handle || target.label}${nested ? ` (+${nested} nested)` : ""}`,
          };
        } catch (error) {
          return {
            invalidate: true,
            notice: `Archive failed: ${error instanceof Error ? error.message : String(error)}`,
          };
        }
      }
      return { invalidate: true };
    }

    if (key.name === "escape") {
      return { invalidate: false, commands: [{ command: "ui.release" }] };
    }
    if (key.ctrl || key.meta) return { invalidate: false };

    if (key.name === "up" || key.sequence === "k") {
      if (!rows.length) return { invalidate: false };
      const idx = Math.max(0, rows.findIndex((row) => row.sessionId === selected?.sessionId));
      ui.selectedSessionId = rows[(idx - 1 + rows.length) % rows.length]!.sessionId;
      return { invalidate: true };
    }
    if (key.name === "down" || key.sequence === "j") {
      if (!rows.length) return { invalidate: false };
      const idx = Math.max(0, rows.findIndex((row) => row.sessionId === selected?.sessionId));
      ui.selectedSessionId = rows[(idx + 1) % rows.length]!.sessionId;
      return { invalidate: true };
    }
    if (isEnter(key) && selected) {
      return {
        invalidate: true,
        commands: [
          {
            command: "pane.open",
            pane: "subagent-viewer",
            instance: selected.sessionId,
            title: `Subagent · ${selected.label}`,
            slot: "right",
            focus: false,
          },
          {
            command: "pane.pin",
            pane: "subagent-viewer",
            instance: selected.sessionId,
            pinned: true,
          },
        ],
      };
    }
    if ((key.sequence === "v" || key.sequence === "o") && selected) {
      return {
        invalidate: false,
        commands: [
          {
            command: "pane.open",
            pane: "subagent-viewer",
            instance: selected.sessionId,
            title: `Subagent · ${selected.label}`,
            slot: "right",
            focus: true,
          },
        ],
      };
    }
    if (key.sequence === "d" && selected) {
      if (selected.archived) {
        return {
          invalidate: false,
          notice: `${selected.label} is already archived · use /subagents steer ${selected.sessionId} <text> to wake it`,
        };
      }
      ui.confirmArchiveSessionId = selected.sessionId;
      return { invalidate: true };
    }
    if (key.sequence === "a") {
      ui.showArchived = !ui.showArchived;
      return { invalidate: true };
    }
    if (key.sequence === "r") {
      const sessionId = context.session?.id;
      if (!sessionId) return { invalidate: false, notice: "No active session" };
      try {
        ui.apiSnapshot = await api.bundleApi.get<MultiAgentSnapshot>("/members", { sessionId });
        return { invalidate: true, notice: "Refreshed subagents" };
      } catch (error) {
        return {
          invalidate: false,
          notice: `Refresh failed: ${error instanceof Error ? error.message : String(error)}`,
        };
      }
    }
    if (key.sequence === "q") {
      return { invalidate: false, commands: [{ command: "pane.close", pane: "subagents" }] };
    }
    return { invalidate: false };
  };

  api.registerPane({
    id: "subagents",
    title: "Subagents",
    defaultSlot: "right",
    defaultSize: { width: "30%", minWidth: 20 },
    pinnable: true,
    closable: true,
    render: ({ context, width }) => render(context, width),
    onKey: (key, context) => onKey(key, context),
    onAction: (action) => {
      const data = typeof action.data === "object" && action.data !== null ? (action.data as Record<string, unknown>) : undefined;
      const sessionId = typeof data?.sessionId === "string" ? data.sessionId : undefined;
      if (!sessionId) return { invalidate: false };
      ui.selectedSessionId = sessionId;
      return {
        invalidate: true,
        commands: [
          {
            command: "pane.open",
            pane: "subagent-viewer",
            slot: "right",
            focus: false,
          },
        ],
      };
    },
  });
}
