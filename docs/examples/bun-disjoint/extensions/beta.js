export default {
  id: "docs-bun-beta-extension",
  server: async () => ({
    tool: {
      beta: {
        description: "Return a deterministic beta marker.",
        execute: async () => "beta",
      },
    },
    "tool.execute.before": async () => {},
  }),
};
