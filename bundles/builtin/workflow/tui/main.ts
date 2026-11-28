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
    registerWorkflowsPane(api, ui);
    registerWorkflowCommands(api, ui);
  },
});
