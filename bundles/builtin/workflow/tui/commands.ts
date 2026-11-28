/**
 * `/workflow` and `/workflows` slash commands contributed by `hya/workflow`.
 * Calls `GET /workflows`, `GET /state`, and `POST /command` via `api.bundleApi`
 * and opens/closes the `hya/workflow#workflows` dynamic pane.
 */
import type { ExtensionContext, HandlerResult, TuiExtensionApi } from "@hya/tui-sdk";
import { refreshWorkflows, workflowName, type WorkflowProjectionView, type WorkflowUiState } from "./workflows";

const usage = "Usage: /workflow [list | state | info <name> | select <name> | run [name] | close]";

export function registerWorkflowCommands(api: TuiExtensionApi, ui: WorkflowUiState): void {
  api.registerCommand({
    name: "workflows",
    description: "Open the Workflows pane and list available workflows",
    execute: async (_input, context: ExtensionContext): Promise<HandlerResult> => {
      await refreshWorkflows(api, ui, context);
      return {
        invalidate: true,
        notice: `Workflows: ${ui.workflows.length} available`,
        commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: true }],
      };
    },
  });

  api.registerCommand({
    name: "workflow",
    description: "List, inspect, select, or run a workflow in the current session",
    argumentHint: "[list | state | info <name> | select <name> | run [name] | close]",
    execute: async ({ args }, context: ExtensionContext): Promise<HandlerResult> => {
      const tokens = args.trim() ? args.trim().split(/\s+/) : [];
      const action = tokens[0] ?? "";
      const name = tokens[1];
      const sessionId = context.session?.id;

      if (!action || action === "open") {
        await refreshWorkflows(api, ui, context);
        return {
          invalidate: true,
          commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: true }],
        };
      }

      if (action === "close") {
        return {
          invalidate: false,
          commands: [{ command: "pane.close", pane: "workflows" }],
        };
      }

      if (action === "list") {
        await refreshWorkflows(api, ui, context);
        const names = ui.workflows.map(workflowName);
        return {
          invalidate: true,
          notice: names.length ? `Workflows: ${names.join(", ")}` : "No workflows available",
          commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: false }],
        };
      }

      if (!sessionId) {
        return { invalidate: false, notice: "Open a session before controlling workflows" };
      }

      if (action === "state") {
        const state = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId });
        ui.apiState = state;
        const selected = state?.selection?.name || state?.selection?.id || "none";
        const runStatus = state?.run?.status ? ` · run ${state.run.status}` : "";
        return {
          invalidate: true,
          notice: `Workflow state: selected=${selected}${runStatus}`,
          commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: false }],
        };
      }

      if (action === "info") {
        if (!name) return { invalidate: false, notice: "Usage: /workflow info <name>" };
        const reply = await api.bundleApi.post<{ info?: { name?: string; description?: string } }>(
          "/command",
          { info: { name } },
          { sessionId },
        );
        const desc = reply?.info?.description ? ` — ${reply.info.description}` : "";
        return { invalidate: false, notice: `Workflow ${reply?.info?.name || name}${desc}` };
      }

      if (action === "select") {
        if (!name) return { invalidate: false, notice: "Usage: /workflow select <name>" };
        await api.bundleApi.post("/command", { select: { name } }, { sessionId });
        ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId }).catch(() => ui.apiState);
        return {
          invalidate: true,
          notice: `Workflow select submitted (${name})`,
          commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: false }],
        };
      }

      if (action === "run") {
        await api.bundleApi.post("/command", { run: name ? { name } : {} }, { sessionId });
        ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId }).catch(() => ui.apiState);
        return {
          invalidate: true,
          notice: `Workflow run submitted${name ? ` (${name})` : ""}`,
          commands: [{ command: "pane.open", pane: "workflows", slot: "right", focus: false }],
        };
      }

      return { invalidate: false, notice: usage };
    },
  });
}
