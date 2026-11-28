/**
 * `hya/plan` TUI extension (`@hya/tui-sdk` v2):
 * - Renders `plan_exit` (and alias `plan`) tool calls as a dedicated approval card.
 * - Contributes a `Plan` status badge while the open session runs the `hya-plan` agent.
 */
import { defineTuiExtension, type ToolCardPresentation, type ToolCallInput, type TuiExtensionApi } from "@hya/tui-sdk";

function renderPlanExitCard(input: ToolCallInput): ToolCardPresentation {
  const statusMeta = (() => {
    switch (input.status) {
      case "completed":
        return "approved · switching to build agent";
      case "error":
        return "rejected";
      case "running":
        return "awaiting approval to exit plan mode";
      default:
        return "preparing plan exit";
    }
  })();

  const output = input.output;
  const result = output && typeof output === "object" && "output" in output ? output.output : output;
  const detail = typeof result === "string" && result.trim()
    ? result.trim()
    : input.status === "completed"
      ? "User approved switching from plan mode to the build agent."
      : input.status === "running"
        ? "Confirm in the prompt dock to switch to the build agent."
        : input.summary || "Requesting transition out of plan mode.";

  return {
    summary: "Plan Mode Exit",
    statusChip: statusMeta,
    expandedByDefault: true,
    ...(input.error ? {} : { body: { kind: "text", text: detail, style: { color: "muted" } } }),
  };
}

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    api.registerToolRenderer({
      id: "plan-exit-card",
      tools: ["plan_exit", "plan", "plan__plan_exit"],
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
