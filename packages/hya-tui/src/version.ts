/** The TUI's frontend version and backend compatibility contract. */
import { frontendVersion, isBackendVersionSupported, minimumBackendVersion } from "../frontend-version"

export const tuiVersion: string = frontendVersion

/** Return a user-facing error when a backend cannot serve this frontend. */
export function backendVersionError(version: string): string | undefined {
  if (isBackendVersionSupported(version)) return undefined
  const actual = version || "unknown"
  return `backend ${actual} is incompatible with frontend ${tuiVersion}; requires backend >= ${minimumBackendVersion}`
}
