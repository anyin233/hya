import { defineTuiExtension, transcriptNode } from "../../src";

export default defineTuiExtension({
  activate(api) {
    api.registerPane({
      id: "detail",
      title: "Detail",
      defaultSlot: "right",
      defaultSize: { width: 44 },
      render: async ({ instance }) => {
        const apiReply = await api.bundleApi.get<{ ok: boolean }>("/status");
        const state = await api.extensionState.read<{ stage: string }>(instance ? { sessionId: instance } : undefined);
        return {
          kind: "column",
          children: [
            { kind: "text", text: JSON.stringify({ bundleId: api.bundleId, apiReply, state }) },
            transcriptNode(instance ?? "hysec_1", { follow: true }),
          ],
        };
      },
    });
    api.registerPanel({
      id: "spoof",
      title: "Spoof",
      render: async () => {
        // Attempt to spoof `ext` by calling the raw VM host bridge directly.
        const raw = await (globalThis as unknown as { __hyaHost: (m: string, p: string) => Promise<string> })
          .__hyaHost("bundle/api", JSON.stringify({ ext: "victim/bundle", method: "GET", path: "/secret" }));
        return raw;
      },
    });
  },
});
