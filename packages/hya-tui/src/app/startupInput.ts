/** What the startup list shows right now (state/startup.ts): the store's catalogs and the extension manager's state. */
import { extensionManager } from "../extensions/manager"
import type { StartupInput } from "../state/startup"
import type { AppState } from "../state/store"

export function startupInput(state: AppState): StartupInput {
  const catalog = extensionManager.catalogStatus()
  return {
    notices: state.startupNotices,
    ...(state.ready ? { skills: state.skills, skillDiagnostics: state.skillDiagnostics, commands: state.backendCommands, ...(state.contextFiles ? { contextFiles: state.contextFiles } : {}) } : {}),
    ...(state.startupReady ? { mcp: state.mcpServers, bundles: state.bundles } : {}),
    ...(catalog.loaded ? { extensions: extensionManager.list() } : {}),
    ...(catalog.error ? { extensionsError: catalog.error } : {}),
  }
}
