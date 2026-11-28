import { defineTuiExtension, type TuiExtensionApi } from "@hya/tui-sdk";
import { registerChannelRenderers } from "./renderers";
import { registerChannelStatusItem } from "./status";

export default defineTuiExtension({
  activate(api: TuiExtensionApi): void {
    registerChannelRenderers(api);
    registerChannelStatusItem(api);
  },
});
