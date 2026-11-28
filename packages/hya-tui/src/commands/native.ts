/** Registration order of the built-in slash commands. */
import { revertReadOnlyStatus } from "../app/revert"
import type { CommandSummary } from "../client"
import { CommandRegistry, type CommandContext, type CommandInvocation, type CommandSpec } from "./registry"
import { keybindingsCommand } from "./keybindings"
import { configCommand, pasteCommand, resourcesCommand, setCommand } from "./config"
import { closestCommand, looksLikePath, skillCommandPrefix } from "./menu"
import { reloadCommand, reloadTargetCommands } from "./reload"

import { apiCommand } from "./native/api"
import { statusCommand, connectionCommands } from "./native/connection"
import { exitCommands } from "./native/exit"
import { extensionsCommand, catalogCommands } from "./native/extensions"
import { permissionModeCommand, interactionCommands } from "./native/interactions"
import { modelCommands, keyCommand } from "./native/models"
import { helpCommand, presentationCommands } from "./native/presentation"
import { sessionNavigationCommands, sessionHistoryCommands } from "./native/sessions"
import { layoutCommand, projectCommands } from "./native/workspace"

export { backendText } from "./native/connection"
export { setExtensionTrust } from "./native/extensions"
export { toBackground } from "./native/exit"
export { openEffortPicker, openModelPicker, selectAgent, selectEffort } from "./native/models"
export { sessionPickerActions } from "./native/sessions"

/** Run an unregistered `/name args` as a backend command turn, creating a session if needed. */
/**
 * Whether `name` (no slash) is in the backend command catalog. A miss
 * re-reads `ListCommands` once (a command or skill added since the last
 * read); `undefined` when that read fails, so the command still goes to the
 * backend as before.
 */
async function knownBackendCommand({ store, client }: CommandContext, name: string): Promise<boolean | undefined> {
  if (store.state.backendCommands.some((command) => command.name === name)) return true
  let commands: CommandSummary[]
  try {
    // The open session's catalog: its agent's bundle skills are commands too.
    commands = await client.listCommands(store.state.selected?.id)
  } catch {
    return undefined
  }
  store.setBackendCommands(commands)
  return commands.some((command) => command.name === name)
}

/**
 * The refusal of an unknown command line, with the closest name. An unknown
 * `/skill:<name>` points at `/skills`; a bare skill name (the skill commands
 * of older releases) at its `/skill:<name>`.
 */
export function unknownCommandMessage(name: string, localNames: readonly string[], backend: readonly CommandSummary[]): string {
  if (name.startsWith(skillCommandPrefix)) return `Unknown skill ${name.slice(skillCommandPrefix.length) || "(no name)"} · /skills lists them`
  const closest = closestCommand(name, [...localNames.map((local) => local.slice(1)), ...backend.map((command) => command.name)])
  return `Unknown command /${name}${closest ? ` · did you mean /${closest}?` : ""} · / lists commands`
}

export async function backendCommand(context: CommandContext, invocation: CommandInvocation): Promise<void> {
  const { store, client, actions } = context
  const name = invocation.name.slice(1)
  if (await knownBackendCommand(context, name) === false) {
    // `/usr/lib/x fails to load` is a prompt that starts with a path, not a command.
    if (looksLikePath(invocation.name) && actions.sendPrompt) {
      await actions.sendPrompt(invocation.text)
      return
    }
    // In the transcript too: the refusal is the whole answer to what was typed.
    const message = unknownCommandMessage(name, context.registry?.names() ?? [], store.state.backendCommands)
    store.addTranscriptNotice(message)
    throw new Error(message)
  }
  if (store.state.selected?.parent) {
    store.setStatus(revertReadOnlyStatus)
    return
  }
  if (!store.state.selected) await actions.newSession()
  const selected = store.state.selected
  if (!selected) throw new Error("Session creation failed")
  const turn = await client.createCommandTurn(selected.id, name, invocation.argumentsText)
  // `CreateTurn` returns the user message id as the turn id for a command
  // turn (same as a prompt turn); the transcript shows what the user typed
  // instead of the backend's expanded template text (state/messages.ts).
  // A skill's `<skill>` block is folded from the message text itself
  // (state/skillBlock.ts), the same in every client.
  if (!name.startsWith(skillCommandPrefix)) store.rememberCommand(turn.id, invocation.text)
  store.setTurn(turn.id)
  store.setStatus(turn.id ? `Command ${name} · ${turn.state.toLowerCase()}` : `Command ${name} finished`)
  actions.scheduleRefresh()
}


export const nativeCommandSpecs: CommandSpec[] = [
  keybindingsCommand,
  configCommand,
  setCommand,
  pasteCommand,
  resourcesCommand,
  reloadCommand,
  ...reloadTargetCommands,
  layoutCommand,
  extensionsCommand,
  helpCommand,
  ...sessionNavigationCommands,
  ...modelCommands,
  ...sessionHistoryCommands,
  statusCommand,
  permissionModeCommand,
  keyCommand,
  ...catalogCommands,
  ...projectCommands,
  ...interactionCommands,
  ...connectionCommands,
  ...presentationCommands,
  ...exitCommands,
  apiCommand,
]

/** A registry holding every native command; unknown names run as backend command turns. */
export function createCommandRegistry(): CommandRegistry {
  const registry = new CommandRegistry(backendCommand)
  for (const spec of nativeCommandSpecs) registry.register(spec)
  return registry
}
