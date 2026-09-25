/** Command completion and concealed key entry for the OpenTUI frontend. */

export const nativeCommands = [
  "/new", "/sessions", "/open", "/models", "/model", "/workflows", "/workflow",
  "/interactions", "/approve", "/deny", "/answer", "/cancel", "/refresh",
  "/api", "/help", "/keys", "/key", "/login",
]

export interface CompletionContext {
  backendCommands: string[]
  providers: string[]
  savedKeys: string[]
  models: string[]
  sessions: string[]
  workflows: string[]
  interactions: string[]
  agents: string[]
  apiOperations: string[]
}

function matches(head: string, prefix: string, values: string[]): string[] {
  return [...new Set(values)]
    .filter((value) => value.toLowerCase().startsWith(prefix.toLowerCase()))
    .sort((a, b) => a.localeCompare(b))
    .map((value) => `${head}${value}`)
}

/** Return full replacement values for the current command line. */
export function completeCommand(input: string, context: CompletionContext): string[] {
  if (!input.startsWith("/")) return []
  const space = input.indexOf(" ")
  if (space < 0) {
    return matches("", input, [
      ...nativeCommands,
      ...context.backendCommands.map((name) => `/${name}`),
    ])
  }
  const command = input.slice(0, space)
  const rest = input.slice(space + 1)
  const words = rest.split(" ")
  const current = words.at(-1) ?? ""
  const head = input.slice(0, input.length - current.length)
  switch (command) {
    case "/key":
      if (words.length === 1) return matches(head, current, ["set", "remove"])
      if (words.length === 2 && words[0] === "set") return matches(head, current, context.providers)
      if (words.length === 2 && words[0] === "remove") return matches(head, current, context.savedKeys)
      return []
    case "/login": return words.length === 1 ? matches(head, current, context.providers) : []
    case "/model": return words.length === 1 ? matches(head, current, context.models) : []
    case "/open": return words.length === 1 ? matches(head, current, context.sessions) : []
    case "/new":
      if (words.length === 1) return matches(head, current, context.agents)
      if (words.length === 2) return matches(head, current, context.models)
      return []
    case "/workflow":
      if (words.length === 1) return matches(head, current, ["select", "run"])
      if (words.length === 2 && ["select", "run"].includes(words[0] ?? "")) {
        return matches(head, current, context.workflows)
      }
      return []
    case "/approve":
    case "/deny":
    case "/answer":
      return words.length === 1 ? matches(head, current, context.interactions) : []
    case "/api": {
      if (words.length === 1) return matches(head, current, ["GET", "POST", "PUT", "PATCH", "DELETE"])
      if (words.length === 2) {
        const method = words[0]?.toUpperCase()
        return context.apiOperations
          .filter((operation) => operation.startsWith(`${method} `))
          .map((operation) => operation.slice(method.length + 1))
          .filter((path) => path.toLowerCase().startsWith(current.toLowerCase()))
          .sort()
          .map((path) => `${head}${path}`)
      }
      return []
    }
    default: return []
  }
}

/** Holds a provider key outside any renderable or command string. */
export class SecretEntry {
  private value = ""

  get mask(): string { return "•".repeat(this.value.length) }

  append(text: string): void {
    const clean = text.replace(/[\r\n\x00-\x1f\x7f]/g, "")
    this.value += clean.slice(0, Math.max(0, 4096 - this.value.length))
  }

  backspace(): void { this.value = this.value.slice(0, -1) }

  take(): string {
    const result = this.value.trim()
    this.value = ""
    return result
  }

  clear(): void { this.value = "" }
}
