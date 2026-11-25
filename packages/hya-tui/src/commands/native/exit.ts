import { webTabBackgroundNotice } from "../../state/format"
import { type CommandContext, type CommandSpec } from "../registry"

/**
 * `/to-background` and Ctrl+D: quit at once and leave the session running on
 * the daemon (no archive). In a WebUI tab (`--web-tab`) closing the tab
 * already does that, so this only says so.
 */
export function toBackground({ store, actions }: CommandContext): void {
  if (store.state.webTab) {
    store.setStatus(webTabBackgroundNotice)
    return
  }
  actions.quit("background")
}

export const exitCommands: CommandSpec[] = [
  {
    name: "/exit",
    description: "Quit the TUI and archive the session (Ctrl+C twice); an unused one is dropped by the daemon. /resume brings an archived one back",
    run: ({ actions }) => { actions.quit("archive") },
  },
  {
    name: "/quit",
    description: "Alias for /exit",
    run: ({ actions }) => { actions.quit("archive") },
  },
  {
    name: "/to-background",
    description: "Quit the TUI at once and leave the session running on the backend daemon, not archived (Ctrl+D on an empty input). Not in a WebUI tab: close the tab instead",
    terminalOnly: true,
    run: (context) => { toBackground(context) },
  },
]
