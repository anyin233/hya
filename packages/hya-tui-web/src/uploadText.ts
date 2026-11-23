// Browser-safe helpers of the upload flow (ADR-0037), shared by the page and its tests.

/** The text the page pastes for stored paths: each single-quoted, separated by spaces (what a native terminal pastes for dragged files). */
export function pastedPathsText(paths: readonly string[]): string {
  return paths.map((path) => `'${path.replace(/'/g, `'\\''`)}'`).join(" ")
}
