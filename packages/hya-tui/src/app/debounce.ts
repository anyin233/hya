/**
 * A trailing debounce with a maximum wait: `schedule()` calls coalesce into
 * one run `wait` ms after the last call, but a steady stream of calls still
 * runs at least every `maxWait` ms. The controller uses it for projection
 * re-reads, which a busy event stream must not postpone indefinitely.
 */

export interface Timers {
  setTimeout(fn: () => void, ms: number): unknown
  clearTimeout(id: unknown): void
  now(): number
}

const realTimers: Timers = {
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
  now: () => Date.now(),
}

export function createDebounce(fn: () => void, options: { wait: number; maxWait: number; timers?: Timers }) {
  const timers = options.timers ?? realTimers
  let timer: unknown
  let firstAt: number | undefined

  function run(): void {
    timer = undefined
    firstAt = undefined
    fn()
  }

  return {
    schedule(): void {
      const now = timers.now()
      firstAt ??= now
      if (timer !== undefined) timers.clearTimeout(timer)
      timer = timers.setTimeout(run, Math.max(0, Math.min(options.wait, firstAt + options.maxWait - now)))
    },
    cancel(): void {
      if (timer !== undefined) timers.clearTimeout(timer)
      timer = undefined
      firstAt = undefined
    },
  }
}
