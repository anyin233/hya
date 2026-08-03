export default {
  id: "docs-bun-alpha-extension",
  server: async () => ({
    tool: {
      echo: {
        description: "Return input as deterministic JSON text.",
        execute: async (input) => JSON.stringify(input) ?? "",
      },
    },
    event: async () => {},
  }),
};
