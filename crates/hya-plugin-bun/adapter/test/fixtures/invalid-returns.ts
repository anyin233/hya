export default {
  server() {
    return { "chat.params": () => null, "tool.execute.after": () => ({ status: "ok" }) }
  },
}
