import { defineTuiExtension, type TuiExtensionApi } from "@hya/tui-sdk";
import { createUiState } from "./state";
import { registerSubagentsPane } from "./subagents";
import { registerSubagentViewer } from "./viewer";
import { registerSubagentCommands } from "./commands";

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    const ui = createUiState();
    api.onContext((context) => {
      const nextSession = context.session?.id;
      if (nextSession !== ui.lastSessionId) {
        ui.lastSessionId = nextSession;
        ui.apiSnapshot = undefined;
        ui.confirmArchiveSessionId = undefined;
      }
      return true;
    });
    registerSubagentsPane(api, ui);
    registerSubagentViewer(api, ui);
    registerSubagentCommands(api, ui);
  },
});
