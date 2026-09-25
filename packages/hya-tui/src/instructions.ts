/** One persistent instruction line for the current TUI view. */
export type View = "chat" | "models" | "workflows" | "interactions" | "keys" | "connect" | "api" | "help"

export function footerInstruction(view: View, savedKeysAvailable: boolean, enteringKey = false): string {
  if (enteringKey) return "Paste API key · Enter saves · Esc cancels"

  switch (view) {
    case "chat": return "Enter a prompt · /new creates a session · /help lists commands"
    case "models": return "Next: /model <provider/model> to switch this session · /help"
    case "workflows": return "Next: /workflow select <name> or /workflow run [name]"
    case "interactions": return "Next: /approve <id>, /deny <id>, or /answer <id> <text>"
    case "keys": return savedKeysAvailable
      ? "Next: /connect deepseek · /key set <provider> · /key remove <provider>"
      : "Next: restart backend 0.37.6+ to list saved keys · /help"
    case "connect": return "Enter saves route · Esc cancels · /connect custom <id> <base-url> <model-id>"
    case "api": return "Next: /api GET /v1/health · /help for command syntax"
    case "help": return "Enter a prompt or choose a /command · Tab completes"
  }
}
