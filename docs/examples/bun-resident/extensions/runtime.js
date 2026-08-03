export default {
  id: "docs-bun-resident-runtime",
  server: async () => ({
    tool: {
      echo: {
        description: "Return the provided value as JSON.",
        execute: async (input) => JSON.stringify(input) ?? "",
      },
    },
  }),
};
