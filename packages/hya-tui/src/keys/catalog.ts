/** Inspectable action catalog for `/keybind`; shortcuts stay in bindings.ts. */
import { keyBindings, type KeyAction } from "./bindings"

export const bindingScopes = ["workspace", "conversation", "pane"] as const
export type BindingScope = (typeof bindingScopes)[number]

export interface BindingSetting {
  /** Stable action identifier; opening an entry never executes the action. */
  id: KeyAction
  scope: BindingScope
  /** Related slash command, when there is one. Key-specific guards still apply. */
  command?: string
  keys: string[]
  description: string
  context: string
}

interface ActionInfo {
  scope: BindingScope
  command?: string
}

// Scope describes the current Composer router, not a new dispatch mechanism.
const actionInfo: Record<KeyAction, ActionInfo> = {
  interrupt: { scope: "conversation", command: "/cancel" },
  quit: { scope: "workspace", command: "/exit" },
  eof: { scope: "workspace", command: "/to-background" },
  complete: { scope: "conversation" },
  openCommands: { scope: "workspace" },
  focusPaneLeft: { scope: "workspace", command: "/layout focus left" },
  focusPaneRight: { scope: "workspace", command: "/layout focus right" },
  focusPaneUp: { scope: "workspace", command: "/layout focus up" },
  focusPaneDown: { scope: "workspace", command: "/layout focus down" },
  cycleMode: { scope: "conversation", command: "/permissions" },
  refresh: { scope: "workspace", command: "/refresh" },
  toggleSidebar: { scope: "workspace", command: "/sidebar" },
  toggleProjectsSidebar: { scope: "workspace", command: "/projects-sidebar" },
  toggleThinking: { scope: "conversation", command: "/thinking" },
  toggleTools: { scope: "conversation", command: "/tools" },
  pageUp: { scope: "pane" },
  pageDown: { scope: "pane" },
  scrollTop: { scope: "pane" },
  scrollBottom: { scope: "pane" },
  help: { scope: "workspace", command: "/help" },
  chord: { scope: "workspace" },
  externalEditor: { scope: "conversation", command: "/editor" },
  undo: { scope: "conversation", command: "/undo" },
  redo: { scope: "conversation", command: "/redo" },
  fork: { scope: "conversation", command: "/fork" },
  reviewPending: { scope: "workspace", command: "/pending" },
}

const contexts: Record<BindingScope, string> = {
  workspace: "Workspace action across tiled panes. Open modals and command input take precedence; individual key conditions still apply.",
  conversation: "Conversation focus only. Other panes keep these keys; prompts, completion and Vim can take precedence.",
  pane: "Scroll the focused pane, except Projects, which owns its navigation. In conversation focus, scroll the transcript; plain Home/End require an empty message input.",
}

/** One entry per action; unassigned actions retain their command and scope metadata. */
export function bindingSettings(): BindingSetting[] {
  const entries = new Map<KeyAction, BindingSetting>()
  for (const binding of keyBindings) {
    const info = actionInfo[binding.action]
    const entry = entries.get(binding.action) ?? {
      id: binding.action, ...info, keys: [], description: "", context: contexts[info.scope],
    }
    entry.keys.push(binding.label)
    entry.description += `${entry.description ? " " : ""}${binding.description}`
    entries.set(binding.action, entry)
  }
  for (const [id, info] of Object.entries(actionInfo) as [KeyAction, ActionInfo][]) {
    if (!entries.has(id)) entries.set(id, {
      id, ...info, keys: [], description: "No default shortcut; use the command or /keybind set.", context: contexts[info.scope],
    })
  }
  return [...entries.values()]
}

/** Exact action ID or related command; no command is run by this lookup. */
export function findBindingSetting(target: string): BindingSetting | undefined {
  return bindingSettings().find((entry) => entry.id === target || entry.command === target)
}
