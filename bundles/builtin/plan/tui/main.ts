/**
 * `hya/plan` TUI extension (`@hya/tui-sdk` v2):
 * - Renders `plan_exit` (and alias `plan`) tool calls as a dedicated approval card.
 * - Contributes a `Plan` status badge while the open session runs the `hya-plan` agent.
 */
import { defineTuiExtension, type RenderNode, type ToolCallInput, type TuiExtensionApi } from "@hya/tui-sdk";

function isPlanExitTool(tool: string): boolean {
  return tool === "plan_exit" || tool === "plan" || tool === "plan__plan_exit";
}

function renderPlanExitCard(input: ToolCallInput): RenderNode | null {
  if (!isPlanExitTool(input.tool)) return null;

  const statusMeta = (() => {
    switch (input.status) {
      case "completed":
        return { glyph: "✓", label: "approved · switching to build agent", color: "success" };
      case "error":
        return { glyph: "✗", label: "rejected", color: "warning" };
      case "running":
        return { glyph: "◌", label: "awaiting approval to exit plan mode", color: "warning" };
      default:
        return { glyph: "○", label: "preparing plan exit", color: "muted" };
    }
  })();

  const detail =
    input.error
      ? input.error
      : typeof input.output === "string" && input.output.trim()
        ? input.output.trim()
        : input.status === "completed"
          ? "User approved switching from plan mode to the build agent."
          : input.status === "running"
            ? "Confirm in the prompt dock to switch to the build agent."
            : input.summary || "Requesting transition out of plan mode.";

  return {
    kind: "box",
    title: "Plan Mode Exit",
    children: [
      {
        kind: "row",
        children: [
          { kind: "text", text: `${statusMeta.glyph} `, style: { color: statusMeta.color, bold: true } },
          { kind: "text", text: statusMeta.label, style: { color: statusMeta.color, bold: true } },
        ],
      },
      {
        kind: "text",
        text: detail,
        style: { color: input.status === "error" ? "error" : "muted" },
      },
    ],
  };
}

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    api.registerRenderer({
      id: "plan-exit-card",
      target: "tool_call",
      mode: "replace",
      priority: 10,
      render: (input) => renderPlanExitCard(input),
    });

    api.registerStatusItem({
      id: "plan-mode",
      label: "Plan",
      priority: 2,
      render: ({ context }) => {
        const agent = context.session?.agent ?? "";
        if (agent === "hya-plan" || agent === "plan") {
          return "PLAN (read-only)";
        }
        return null;
      },
    });
  },
});
