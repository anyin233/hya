/** One persistent instruction line for the current TUI view. */
export type View = "chat" | "models" | "workflows" | "interactions" | "api" | "help" | "todos" | "status"

/** `childView`: a subagent's session is open read-only (chat view only). The Provider View (`/key`) draws its own key line. */
export function footerInstruction(view: View, childView = false): string {
  if (childView && view === "chat") return "Read-only subagent view · Esc returns to the parent · click a task card or /open <n> to switch"

  switch (view) {
    case "chat": return "Enter a prompt · /new creates a session · /sessions history · /pending requests · / commands"
    case "models": return "Next: /model <provider/model> to switch this session · /key opens the Provider View · /help"
    case "workflows": return "Next: /workflow select <name> or /workflow run [name]"
    case "interactions": return "Next: /approve <id>, /deny <id>, or /answer <id> <text>"
    case "api": return "Next: /api GET /v1/health · /help for command syntax"
    case "help": return "Esc returns · / opens the command pane · Tab completes there"
    case "todos": return "Next: /refresh to reload the list · /help"
    case "status": return "Next: /model, /agent, or /rename to change what's shown · /help"
  }
}
