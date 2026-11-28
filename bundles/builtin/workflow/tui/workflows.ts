/**
 * `hya/workflow#workflows` dynamic pane: displays discovered workflows, the
 * active session's selected workflow, and live stage/run progress folded from
 * `Projection::extensions["hya/workflow"]` and refreshed via `bundle.api`
 * (`GET /workflows`, `GET /state`, `POST /command`).
 */
import type { ExtensionContext, HandlerResult, JsonValue, KeyEvent, RenderNode, TuiExtensionApi } from "@hya/tui-sdk";

export interface WorkflowSummaryItem {
  readonly id?: string;
  readonly name?: string;
  readonly title?: string;
  readonly description?: string;
  readonly revision?: string;
}

export interface WorkflowStageView {
  readonly plan?: { readonly id?: string; readonly title?: string };
  readonly status?: string;
}

export interface WorkflowRunView {
  readonly id?: string;
  readonly workflow?: { readonly name?: string; readonly id?: string; readonly revision?: string };
  readonly status?: string;
  readonly stages?: readonly WorkflowStageView[];
  readonly error?: string | null;
}

export interface WorkflowProjectionView {
  readonly selection?: { readonly name?: string; readonly id?: string; readonly revision?: string } | null;
  readonly run?: WorkflowRunView | null;
}

export interface WorkflowUiState {
  workflows: readonly WorkflowSummaryItem[];
  highlighted: number;
  apiState?: WorkflowProjectionView;
  lastSessionId?: string;
  lastError?: string;
}

export function createWorkflowUiState(): WorkflowUiState {
  return {
    workflows: [],
    highlighted: 0,
    apiState: undefined,
    lastSessionId: undefined,
    lastError: undefined,
  };
}

const isEnter = (key: KeyEvent): boolean => !key.meta && (key.name === "return" || key.name === "enter" || key.name === "kpenter");

function truncate(text: string, width: number): string {
  if (width <= 0) return "";
  if (text.length <= width) return text;
  return width <= 1 ? "…".slice(0, width) : `${text.slice(0, width - 1)}…`;
}

function asProjection(raw: JsonValue | undefined): WorkflowProjectionView | undefined {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return undefined;
  return raw as unknown as WorkflowProjectionView;
}

export function workflowName(item: WorkflowSummaryItem): string {
  return item.name || item.id || item.title || "(unnamed)";
}

function stageGlyph(status: string | undefined): { glyph: string; color: string } {
  switch ((status ?? "").toLowerCase()) {
    case "completed":
      return { glyph: "✓", color: "success" };
    case "running":
      return { glyph: "◐", color: "activity" };
    case "failed":
      return { glyph: "✗", color: "error" };
    case "cancelled":
    case "skipped":
      return { glyph: "–", color: "warning" };
    default:
      return { glyph: "○", color: "muted" };
  }
}

export async function refreshWorkflows(
  api: TuiExtensionApi,
  ui: WorkflowUiState,
  context: ExtensionContext,
): Promise<void> {
  const directory = context.workspace?.directory;
  try {
    const listed = await api.bundleApi.get<{ workflows?: WorkflowSummaryItem[] }>("/workflows", {
      ...(directory ? { query: { directory } } : {}),
    });
    ui.workflows = Array.isArray(listed?.workflows) ? listed.workflows : [];
    if (ui.highlighted >= ui.workflows.length) ui.highlighted = Math.max(0, ui.workflows.length - 1);
    ui.lastError = undefined;
  } catch (error) {
    ui.lastError = error instanceof Error ? error.message : String(error);
  }
  const sessionId = context.session?.id;
  if (sessionId) {
    try {
      ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId });
    } catch {
      // Fall back to folded extension_state when session state is absent.
    }
  }
}

export function registerWorkflowsPane(api: TuiExtensionApi, ui: WorkflowUiState): void {
  const render = (context: ExtensionContext, width: number): RenderNode => {
    const state = ui.apiState ?? asProjection(api.extensionState.get());
    const children: RenderNode[] = [
      { kind: "text", text: truncate("Workflows", width), style: { color: "heading", bold: true } },
    ];

    const selectedName = state?.selection?.name || state?.selection?.id;
    children.push({
      kind: "text",
      text: truncate(`Selected: ${selectedName ?? "none"}`, width),
      style: { color: selectedName ? "fg" : "muted" },
    });

    const run = state?.run;
    if (run) {
      const stages = run.stages ?? [];
      const done = stages.filter((s) => (s.status ?? "").toLowerCase() === "completed").length;
      const runStatus = (run.status ?? "running").toLowerCase();
      const runColor =
        runStatus === "completed"
          ? "success"
          : runStatus === "failed"
            ? "error"
            : runStatus === "cancelled"
              ? "warning"
              : "activity";
      children.push({
        kind: "text",
        text: truncate(`Run ${run.id ?? ""} · ${runStatus}`, width),
        style: { color: runColor, bold: true },
      });
      if (stages.length > 0) {
        children.push({ kind: "progress", value: done, total: stages.length, label: "Stages" });
        for (const stage of stages) {
          const { glyph, color } = stageGlyph(stage.status);
          const stageLabel = stage.plan?.title || stage.plan?.id || "stage";
          children.push({
            kind: "row",
            children: [
              { kind: "text", text: `${glyph} `, style: { color } },
              {
                kind: "text",
                text: truncate(`${stageLabel} (${(stage.status ?? "pending").toLowerCase()})`, Math.max(4, width - 2)),
                style: { color: "fg" },
              },
            ],
          });
        }
      }
      if (run.error) {
        children.push({
          kind: "text",
          text: truncate(`Error: ${run.error}`, width),
          style: { color: "error" },
        });
      }
    }

    children.push({
      kind: "text",
      text: truncate(`Available (${ui.workflows.length})`, width),
      style: { color: "heading", bold: true },
    });

    if (!ui.workflows.length) {
      children.push({
        kind: "text",
        text: "No workflows loaded · press g or run /workflow list",
        style: { color: "muted" },
      });
    } else {
      ui.workflows.forEach((wf, idx) => {
        const name = workflowName(wf);
        const focused = idx === ui.highlighted;
        const action = { name: "select", data: { name } };
        children.push({
          kind: "row",
          children: [
            { kind: "text", text: focused ? "▸ " : "  ", style: { color: "focus" }, action },
            {
              kind: "text",
              text: truncate(name, Math.max(4, width - 4)),
              style: { color: focused ? "focus" : "fg", ...(focused ? { bold: true } : {}) },
              action,
            },
          ],
        });
        if (wf.description) {
          children.push({
            kind: "text",
            text: truncate(`  ${wf.description}`, width),
            style: { color: "muted" },
            action,
          });
        }
      });
    }

    if (ui.lastError) {
      children.push({
        kind: "text",
        text: truncate(`Error: ${ui.lastError}`, width),
        style: { color: "error" },
      });
    }

    children.push({
      kind: "text",
      text: truncate("↑↓ move · Enter select · r run · g refresh · q close", width),
      style: { color: "muted" },
    });

    return { kind: "column", children };
  };

  const onKey = async (key: KeyEvent, context: ExtensionContext): Promise<HandlerResult> => {
    if (key.name === "escape") return { invalidate: false, commands: [{ command: "ui.release" }] };
    if (key.ctrl || key.meta) return { invalidate: false };

    if (key.name === "up" || key.sequence === "k") {
      if (!ui.workflows.length) return { invalidate: false };
      ui.highlighted = (ui.highlighted - 1 + ui.workflows.length) % ui.workflows.length;
      return { invalidate: true };
    }
    if (key.name === "down" || key.sequence === "j") {
      if (!ui.workflows.length) return { invalidate: false };
      ui.highlighted = (ui.highlighted + 1) % ui.workflows.length;
      return { invalidate: true };
    }
    if (key.sequence === "g") {
      await refreshWorkflows(api, ui, context);
      return { invalidate: true, notice: `Loaded ${ui.workflows.length} workflow(s)` };
    }
    if (isEnter(key)) {
      const target = ui.workflows[ui.highlighted];
      const sessionId = context.session?.id;
      if (!target || !sessionId) return { invalidate: false };
      const name = workflowName(target);
      await api.bundleApi.post("/command", { select: { name } }, { sessionId });
      ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId }).catch(() => ui.apiState);
      return { invalidate: true, notice: `Selected workflow ${name}` };
    }
    if (key.sequence === "r") {
      const target = ui.workflows[ui.highlighted];
      const sessionId = context.session?.id;
      if (!sessionId) return { invalidate: false, notice: "Open a session before running a workflow" };
      const name = target ? workflowName(target) : undefined;
      await api.bundleApi.post("/command", { run: name ? { name } : {} }, { sessionId });
      ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId }).catch(() => ui.apiState);
      return { invalidate: true, notice: `Started workflow${name ? ` ${name}` : ""}` };
    }
    if (key.sequence === "q") {
      return { invalidate: false, commands: [{ command: "pane.close", pane: "workflows" }] };
    }
    return { invalidate: false };
  };

  api.registerPane({
    id: "workflows",
    title: "Workflows",
    defaultSlot: "right",
    defaultSize: { width: "40%", minWidth: 28 },
    pinnable: true,
    closable: true,
    render: ({ context, width }) => render(context, width),
    onKey,
    onAction: async (action, context) => {
      const data = typeof action.data === "object" && action.data !== null ? (action.data as Record<string, unknown>) : undefined;
      const name = typeof data?.name === "string" ? data.name : undefined;
      const sessionId = context.session?.id;
      if (!name || !sessionId) return { invalidate: false };
      const idx = ui.workflows.findIndex((wf) => workflowName(wf) === name);
      if (idx >= 0) ui.highlighted = idx;
      await api.bundleApi.post("/command", { select: { name } }, { sessionId });
      ui.apiState = await api.bundleApi.get<WorkflowProjectionView>("/state", { sessionId }).catch(() => ui.apiState);
      return { invalidate: true, notice: `Selected workflow ${name}` };
    },
  });
}
