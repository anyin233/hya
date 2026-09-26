/** Command completion and concealed key entry (the Provider View's key fields) for the OpenTUI frontend. */

import { createCommandRegistry, nativeCommandSpecs } from "./commands/native"
import { matchValues, type CommandRegistry } from "./commands/registry"

/** Names of the built-in slash commands (from the command registry). */
export const nativeCommands = nativeCommandSpecs.map((spec) => spec.name)

export interface CompletionContext {
  backendCommands: string[]
  models: string[]
  sessions: string[]
  workflows: string[]
  interactions: string[]
  agents: string[]
  apiOperations: string[]
  /** Permission mode ids (`/permissions <mode>`). */
  permissionModes?: string[]
  /** In a WebUI tab (`--web-tab`) terminal-only commands (`/to-background`) are not offered. */
  webTab?: boolean
}

const defaultRegistry = createCommandRegistry()

/**
 * Return full replacement values for the current command line: command names
 * (native and backend) before the first space, else the command's own
 * argument completer from the registry.
 */
export function completeCommand(input: string, context: CompletionContext, registry: CommandRegistry = defaultRegistry): string[] {
  if (!input.startsWith("/")) return []
  if (input.indexOf(" ") < 0) {
    return matchValues("", input, [
      ...registry.list().filter((spec) => !(context.webTab && spec.terminalOnly)).map((spec) => spec.name),
      ...context.backendCommands.map((name) => `/${name}`),
    ])
  }
  return registry.complete(input, context)
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

  /** The key without clearing it (a submission that may fail and be retried). */
  peek(): string { return this.value.trim() }

  get length(): number { return this.value.length }

  clear(): void { this.value = "" }
}
