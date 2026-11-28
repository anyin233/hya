import { defineTuiExtension, type TuiExtensionApi } from "@hya/tui-sdk";
import { createWorkflowUiState, registerWorkflowsPane } from "./workflows";
import { registerWorkflowCommands } from "./commands";

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    const ui = createWorkflowUiState();
    api.onContext((context) => {
      const nextSession = context.session?.id;
      if (nextSession !== ui.lastSessionId) {
        ui.lastSessionId = nextSession;
        ui.apiState = undefined;
      }
      return true;
    });
    api.registerToolRenderer({
      id: "workflow-tool",
      tools: ["workflow", "workflow__workflow"],
      render: (call) => {
        const output = call.output;
        const detail = output && typeof output === "object" && "output" in output ? output.output : output;
        const text = typeof detail === "string" ? detail : JSON.stringify(detail, null, 2);
        return { summary: call.summary, ...(text ? { body: { kind: "text", text: text.slice(0, 2048) } } : {}) };
      },
    });
    registerWorkflowsPane(api, ui);
    registerWorkflowCommands(api, ui);
  },
});
