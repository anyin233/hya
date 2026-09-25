/** Command completion and concealed key entry for the OpenTUI frontend. */

import { createCommandRegistry, nativeCommandSpecs } from "./commands/native"
import { matchValues, type CommandRegistry } from "./commands/registry"

/** Names of the built-in slash commands (from the command registry). */
export const nativeCommands = nativeCommandSpecs.map((spec) => spec.name)

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
  /** Permission mode ids (`/permissions <mode>`). */
  permissionModes?: string[]
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
      ...registry.names(),
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

  clear(): void { this.value = "" }
}
