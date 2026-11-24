/** What the startup list shows right now (state/startup.ts): the store's catalogs and the extension manager's state. */
import { extensionManager } from "../extensions/manager"
import type { StartupInput } from "../state/startup"
import type { AppState } from "../state/store"

/**
 * `localNames`: the TUI's own command names (`/status`, …); a project or
 * user command of one of these names is hidden by it, a problem to show.
 */
export function startupInput(state: AppState, localNames: readonly string[] = []): StartupInput {
  const catalog = extensionManager.catalogStatus()
  const shadowedCommands = state.backendCommands.filter((command) => (command.scope === "project" || command.scope === "user") && localNames.includes(`/${command.name}`))
  return {
    notices: state.startupNotices,
    ...(state.ready ? {
      skills: state.skills,
      skillDiagnostics: state.skillDiagnostics,
      commands: state.backendCommands,
      commandDiagnostics: state.commandDiagnostics,
      shadowedCommands,
      ...(state.contextFiles ? { contextFiles: state.contextFiles } : {}),
    } : {}),
    ...(state.startupReady ? { mcp: state.mcpServers, bundles: state.bundles } : {}),
    ...(catalog.loaded ? { extensions: extensionManager.list() } : {}),
    ...(catalog.error ? { extensionsError: catalog.error } : {}),
  }
}
