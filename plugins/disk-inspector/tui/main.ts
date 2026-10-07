import { defineTuiExtension } from "@hya/tui-sdk";
import { PANEL_ID } from "../shared/contracts";
import { renderInspector } from "./panel";

export default defineTuiExtension({
  activate(api) {
    api.registerPanel({
      id: PANEL_ID,
      title: "Disk inspector",
      placement: "pane",
      render: () => renderInspector({ kind: "disconnected" }),
    });
  },
});
