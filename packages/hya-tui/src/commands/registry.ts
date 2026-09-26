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
import type { TuiPreferences } from "../prefs"
import type { PickerSpec } from "../state/picker"
import type { AppStore } from "../state/store"

/** Controller actions a command handler may call. */
export interface AppActions {
  refresh(): Promise<void>
  refreshMessages(): Promise<void>
  openSession(sessionId: string): Promise<void>
  /** Create a session in the active Project (state/projects.ts `sessionPlacement`) and open it; without one on a `--remote` start it sets `noProjectStatus` and throws `NoProjectError`. */
  newSession(agent?: string, model?: string): Promise<void>
  /** Create a temporary session (no Project; the server's scratch workdir) and open it. */
  newTemporarySession(agent?: string, model?: string): Promise<void>
  /** Make a Project active: scope the client to it, open its newest root session or create one there. */
  switchProject(projectId: string): Promise<void>
  /** Re-read `ListProjects` into `state.projects` (also done on every `projectsUpdated` frame). */
  refreshProjects(): Promise<void>
  /** Open the full-screen Provider View (`/key`; state/providers.ts, app/providers.ts). */
  openProviders(): void
  /** Open the full-screen Diff View (`/diff`; state/diff.ts, app/diff.ts). */
  openDiff(): void
  /** Open the full-screen MCP view (`/mcp`; state/mcp.ts, app/mcp.ts). */
  openMcp(): void
  /** Open the full-screen Saved Rules view (`/rules`; state/rules.ts, app/rules.ts). */
  openRules(): void
  /** Open the full-screen Agent Models view (`/agent-models`; state/agentModels.ts, app/agentModels.ts). */
  openAgentModels(): void
  /** Open the full-screen Project view (`/project`, `/projects`; state/projectView.ts, app/projectView.ts). */
  openProjectView(): void
  scheduleRefresh(): void
  /** Cancel the running turn (`CancelTurn`); throws `No active turn` when none runs. */
  cancelTurn(): Promise<void>
  /**
   * Leave the TUI and restore the terminal. `archive` (`/exit`, Ctrl+C twice)
   * archives the open session's root; `background` (`/to-background`,
   * Ctrl+D) leaves it running on the daemon. An empty session this client
   * created is dropped either way (app/sessionKeeper.ts).
   */
  quit(mode: "archive" | "background"): void
  /** `/resume [id]`, `--resume [id]`: unarchive and open a session, or pick one (app/resume.ts). */
  resume(id?: string): Promise<void>
  /** Open the modal picker (components/Picker.tsx); the choice runs `spec.onSelect`. */
  openPicker(spec: PickerSpec): void
  /** Open the key and command help overlay (`/help`, `?`; commands/help.ts). */
  openHelp(): void
  /** Switch the session tree's permission mode (app/modes.ts): yolo asks first once per process; no session → applied on creation. */
  requestPermissionMode(mode: string): Promise<void>
  /** Merge `patch` into the TUI preferences file (src/prefs.ts); throws when it cannot be written. */
  savePreferences(patch: Partial<TuiPreferences>): void
  /** Copy `text` to the system clipboard with OSC 52; `false` when the terminal does not accept it. */
  copyText(text: string): boolean
  /** Edit the composer's input in the external editor (composer/editor.ts); the result goes back into the input. */
  openEditor(): void
  /** `/undo`: revert the last prompt (app/revert.ts). */
  undo(): Promise<void>
  /** `/redo`: undo the pending revert. */
  redo(): Promise<void>
  /** `/fork`: open the fork picker. */
  fork(): void
  /** `/reconnect`: find or start the database's backend now (app/reconnect.ts). */
  reconnect(): Promise<void>
  /**
   * `/connect-remote [<link>] [--transport auto|grpc|ws] [--relay-ca <pem>]`:
   * start a relay bridge child and move to the remote backend behind it
   * (src/bridge.ts); without a link a concealed entry asks for it.
   */
  connectRemote(args: readonly string[]): Promise<void>
  /** `/disconnect-remote`: stop the bridge child and go back to the local backend. */
  disconnectRemote(): Promise<void>
  /**
   * Delete a session (`/sessions` Ctrl+D). Goes through this, not
   * `client.deleteSession` directly: it marks the id so the global stream's
   * echo of this same delete (`docs/protocol/README.md` "Session list push")
   * does not also show the "deleted elsewhere" notice and open a second new
   * session — the picker's own delete flow already navigates.
   */
  deleteSession(id: string): Promise<void>
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
  /** Not offered in a WebUI tab (`--web-tab`): hidden from the command menu, help, and completion; typing it still runs it. */
  terminalOnly?: boolean
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
