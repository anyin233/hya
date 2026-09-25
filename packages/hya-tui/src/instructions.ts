/** One persistent instruction line for the current TUI view. */
export type View = "chat" | "models" | "workflows" | "interactions" | "keys" | "api" | "help" | "todos" | "status"

export function footerInstruction(view: View, savedKeysAvailable: boolean, enteringKey = false): string {
  if (enteringKey) return "Paste API key · Enter saves · Esc cancels"

  switch (view) {
    case "chat": return "Enter a prompt · /new creates a session · /help lists commands · / opens the command menu"
    case "models": return "Next: /model <provider/model> to switch this session · /help"
    case "workflows": return "Next: /workflow select <name> or /workflow run [name]"
    case "interactions": return "Next: /approve <id>, /deny <id>, or /answer <id> <text>"
    case "keys": return savedKeysAvailable
      ? "Next: /key set <provider> to add · /key remove <provider> to delete · Tab completes"
      : "Next: restart backend 0.41.0+ to list saved keys · /help"
    case "api": return "Next: /api GET /v1/health · /help for command syntax"
    case "help": return "Enter a prompt or choose a /command · Tab completes"
    case "todos": return "Next: /refresh to reload the list · /help"
    case "status": return "Next: /model, /agent, or /rename to change what's shown · /help"
  }
}
