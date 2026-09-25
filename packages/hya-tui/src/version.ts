/** This TUI's own version, compared against the backend's bootstrap version (E24 "backend version mismatch" notice). */
import pkg from "../package.json" with { type: "json" }

export const tuiVersion: string = pkg.version
