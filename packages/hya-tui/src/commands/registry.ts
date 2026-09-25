/**
 * Slash-command registry.
 *
 * Each command is one `CommandSpec`: its name, a description, an argument
 * hint, an optional argument completer, and the handler. The registry parses a
 * command line, dispatches it, and completes it. A command line whose name is
 * not registered goes to the fallback handler (backend command catalog).
 */
import type { HyaClient } from "../client"
import type { CompletionContext } from "../completion"
import type { PickerSpec } from "../state/picker"
import type { AppStore } from "../state/store"

/** Controller actions a command handler may call. */
export interface AppActions {
  refresh(): Promise<void>
  refreshMessages(): Promise<void>
  openSession(sessionId: string): Promise<void>
  newSession(agent?: string, model?: string): Promise<void>
  /** Switch the composer into concealed key entry for `provider`. */
  beginKeyEntry(provider: string): void
  scheduleRefresh(): void
  /** Cancel the running turn (`CancelTurn`); throws `No active turn` when none runs. */
  cancelTurn(): Promise<void>
  /** Leave the TUI and restore the terminal. */
  quit(): void
  /** Open the modal picker (components/Picker.tsx); the choice runs `spec.onSelect`. */
  openPicker(spec: PickerSpec): void
  /** Switch the session tree's permission mode (app/modes.ts): yolo asks first once per process; no session → applied on creation. */
  requestPermissionMode(mode: string): Promise<void>
}

export interface CommandContext {
  store: AppStore
  client: HyaClient
  actions: AppActions
}

/** A parsed command line. `argumentsText` is everything after the name, verbatim. */
export interface CommandInvocation {
  name: string
  args: string[]
  argumentsText: string
  text: string
}

/** Where completion is in the argument list. */
export interface ArgumentPosition {
  /** Words after the command name; the last one is being typed. */
  words: string[]
  /** The word being completed. */
  current: string
  /** The input before `current`; completions are prefixed with it. */
  head: string
}

export interface CommandSpec {
  /** Including the leading slash, e.g. `/models`. */
  name: string
  description: string
  argumentHint?: string
  complete?(position: ArgumentPosition, context: CompletionContext): string[]
  run(context: CommandContext, invocation: CommandInvocation): Promise<void> | void
}

/** Full replacement values: `head` + every value starting with `prefix` (case-insensitive), sorted. */
export function matchValues(head: string, prefix: string, values: string[]): string[] {
  return [...new Set(values)]
    .filter((value) => value.toLowerCase().startsWith(prefix.toLowerCase()))
    .sort((a, b) => a.localeCompare(b))
    .map((value) => `${head}${value}`)
}

export class CommandRegistry {
  private readonly specs = new Map<string, CommandSpec>()

  constructor(private readonly fallback: (context: CommandContext, invocation: CommandInvocation) => Promise<void>) {}

  register(spec: CommandSpec): this {
    if (this.specs.has(spec.name)) throw new Error(`Duplicate command ${spec.name}`)
    this.specs.set(spec.name, spec)
    return this
  }

  get(name: string): CommandSpec | undefined { return this.specs.get(name) }

  names(): string[] { return [...this.specs.keys()] }

  list(): CommandSpec[] { return [...this.specs.values()] }

  parse(text: string): CommandInvocation {
    const [name = "", ...args] = text.split(/\s+/)
    return { name, args, argumentsText: text.slice(name.length).trimStart(), text }
  }

  async dispatch(text: string, context: CommandContext): Promise<void> {
    const invocation = this.parse(text)
    const spec = this.specs.get(invocation.name)
    if (spec) await spec.run(context, invocation)
    else await this.fallback(context, invocation)
  }

  /** Complete the argument of a registered command; `[]` for unknown commands. */
  complete(input: string, context: CompletionContext): string[] {
    const space = input.indexOf(" ")
    if (space < 0) return []
    const spec = this.specs.get(input.slice(0, space))
    if (!spec?.complete) return []
    const words = input.slice(space + 1).split(" ")
    const current = words.at(-1) ?? ""
    return spec.complete({ words, current, head: input.slice(0, input.length - current.length) }, context)
  }
}
