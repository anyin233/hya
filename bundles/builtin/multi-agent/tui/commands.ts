/**
 * `/subagents` slash command contributed by `hya/multi-agent`.
 * Calls the bundle's own `BundleApi` (`/members`, `/archive`, `/steer`) and
 * manages the dynamic `subagents` and `subagent-viewer` panes.
 */
import type { ExtensionContext, HandlerResult, TuiExtensionApi } from "@hya/tui-sdk";
import { buildSubagentRows, type MultiAgentSnapshot, type MultiAgentUiState } from "./state";

const usage =
  "Usage: /subagents [list | select <session> | view <session> | pin <session> | follow | archive <session> | steer <session> <text> | archived [show|hide|toggle] | close]";

export function registerSubagentCommands(api: TuiExtensionApi, ui: MultiAgentUiState): void {
  api.registerCommand({
    name: "subagents",
    description: "Inspect and control subagents via hya/multi-agent panes and API",
    argumentHint: "[list|select|view|pin|follow|archive|steer|archived|close]",
    execute: async ({ args }, context: ExtensionContext): Promise<HandlerResult> => {
      const trimmed = args.trim();
      const tokens = trimmed ? trimmed.split(/\s+/) : [];
      const sub = tokens[0] ?? "";
      const activeSessionId = context.session?.id;

      if (!sub) {
        if (!activeSessionId) {
          return { invalidate: false, notice: "Open a conversation before showing subagents" };
        }
        try {
          ui.apiSnapshot = await api.bundleApi.get<MultiAgentSnapshot>("/members", {
            sessionId: activeSessionId,
          });
        } catch {
          // Fallback cleanly to folded extension_state if /members is unavailable.
        }
        return {
          invalidate: true,
          commands: [
            { command: "pane.open", pane: "subagents", slot: "right", focus: true },
            { command: "pane.open", pane: "subagent-viewer", slot: "right", focus: false },
          ],
        };
      }

      if (sub === "close") {
        return {
          invalidate: false,
          commands: [
            { command: "pane.close", pane: "subagents" },
            { command: "pane.close", pane: "subagent-viewer" },
          ],
        };
      }

      if (sub === "archived") {
        const mode = tokens[1];
        if (tokens.length > 2 || (mode && mode !== "show" && mode !== "hide" && mode !== "toggle")) {
          return { invalidate: false, notice: usage };
        }
        ui.showArchived = mode === "show" ? true : mode === "hide" ? false : !ui.showArchived;
        return {
          invalidate: true,
          notice: ui.showArchived ? "Showing archived subagents" : "Hiding archived subagents",
        };
      }

      if (sub === "list") {
        if (!activeSessionId) {
          return { invalidate: false, notice: "Open a conversation before listing subagents" };
        }
        const snapshot = await api.bundleApi.get<MultiAgentSnapshot>("/members", {
          sessionId: activeSessionId,
        });
        ui.apiSnapshot = snapshot;
        const rows = buildSubagentRows(context, api.extensionState.get(), ui);
        const summary = rows.length
          ? rows.map((row) => `${row.label} (${row.status})`).join(", ")
          : "no active subagents";
        return { invalidate: true, notice: `Subagents: ${summary}` };
      }

      if (sub === "select") {
        const target = tokens[1];
        if (!target || tokens.length !== 2) return { invalidate: false, notice: usage };
        ui.selectedSessionId = target;
        return {
          invalidate: true,
          notice: `Selected subagent ${target}`,
          commands: [{ command: "pane.open", pane: "subagent-viewer", slot: "right", focus: false }],
        };
      }

      if (sub === "view") {
        const target = tokens[1];
        if (!target || tokens.length !== 2) return { invalidate: false, notice: usage };
        ui.selectedSessionId = target;
        return {
          invalidate: true,
          commands: [
            {
              command: "pane.open",
              pane: "subagent-viewer",
              instance: target,
              title: `Subagent · ${target}`,
              slot: "right",
              focus: true,
            },
          ],
        };
      }

      if (sub === "pin") {
        const target = tokens[1] ?? ui.selectedSessionId;
        if (!target) return { invalidate: false, notice: usage };
        ui.selectedSessionId = target;
        return {
          invalidate: true,
          commands: [
            {
              command: "pane.open",
              pane: "subagent-viewer",
              instance: target,
              slot: "right",
              focus: false,
            },
            {
              command: "pane.pin",
              pane: "subagent-viewer",
              instance: target,
              pinned: true,
            },
          ],
        };
      }

      if (sub === "follow") {
        return {
          invalidate: true,
          notice: "Subagent viewer now follows selection",
          commands: [{ command: "pane.pin", pane: "subagent-viewer", pinned: false }],
        };
      }

      if (sub === "archive") {
        const target = tokens[1];
        if (!target || tokens.length !== 2) return { invalidate: false, notice: usage };
        const receipt = await api.bundleApi.post<{
          handle?: string;
          descendants?: string[];
        }>("/archive", {}, { sessionId: target });
        if (activeSessionId) {
          ui.apiSnapshot = await api.bundleApi
            .get<MultiAgentSnapshot>("/members", { sessionId: activeSessionId })
            .catch(() => ui.apiSnapshot);
        }
        const nested = Array.isArray(receipt?.descendants) ? receipt.descendants.length : 0;
        return {
          invalidate: true,
          notice: `Archived ${receipt?.handle || target}${nested ? ` and ${nested} nested subagent${nested === 1 ? "" : "s"}` : ""} · steer it to wake it`,
        };
      }

      if (sub === "steer") {
        const target = tokens[1];
        const text = trimmed.replace(/^steer\s+\S+/, "").trim();
        if (!target || !text) return { invalidate: false, notice: usage };
        await api.bundleApi.post("/steer", { text }, { sessionId: target });
        return {
          invalidate: true,
          notice: `Steered ${target}`,
        };
      }

      return { invalidate: false, notice: usage };
    },
  });
}
