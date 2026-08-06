/**
 * Emit structured startup waterfall marks when HYA_STARTUP_TRACE is truthy.
 *
 * Each mark is one JSON line on stderr so parent harnesses can parse wall-clock
 * deltas without depending on process-local clocks.
 */

const enabled = ["1", "true"].includes(process.env.HYA_STARTUP_TRACE?.toLowerCase() ?? "")

const emitted = new Set<string>()

/**
 * Well-known startup mark names used along the TUI bootstrap path.
 *
 * Callers may also pass free-form strings via {@link startupMark}.
 */
export type StartupMark =
  | "bun_entry"
  | "theme_resolved"
  | "shell_paint"
  | "plugin_host_done"
  | "sync_partial"
  | "sync_complete"

/**
 * Record a startup mark. Optional `once` keys emit at most once per process.
 *
 * No-op when `HYA_STARTUP_TRACE` is not truthy. Write failures are swallowed so
 * tracing never takes down the TUI.
 *
 * @param mark - Well-known mark name or free-form string
 * @param detail - Optional free-form detail (URL, mode, etc.)
 * @param options - Emission options
 * @param options.once - Deduplicate by mark name (default true for lifecycle marks)
 * @returns void
 */
export function startupMark(mark: StartupMark | string, detail?: string, options?: { once?: boolean }) {
  if (!enabled) return
  const once = options?.once ?? true
  if (once && emitted.has(mark)) return
  if (once) emitted.add(mark)

  const payload: Record<string, unknown> = {
    hya_startup: true,
    mark,
    wall_ms: Date.now(),
    mono_ms: Math.round(performance.now() * 1000) / 1000,
  }
  if (detail !== undefined && detail.length > 0) {
    payload.detail = detail
  }
  try {
    process.stderr.write(`${JSON.stringify(payload)}\n`)
  } catch {
    // Tracing must never take down the TUI.
  }
}

/**
 * Whether startup tracing is active for this process.
 *
 * @returns true when `HYA_STARTUP_TRACE` is `1` or `true` (case-insensitive)
 */
export function startupTraceEnabled() {
  return enabled
}
