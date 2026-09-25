import { expect, test } from "bun:test"
import { createDebounce } from "../src/app/debounce"

/** A manual clock: timers fire only when `advance` passes their deadline. */
function clock() {
  let now = 0
  let next = 0
  const timers = new Map<number, { at: number; fn: () => void }>()
  return {
    setTimeout: (fn: () => void, ms: number) => { const id = ++next; timers.set(id, { at: now + ms, fn }); return id },
    clearTimeout: (id: number) => { timers.delete(id) },
    now: () => now,
    advance(ms: number) {
      const end = now + ms
      for (;;) {
        const due = [...timers.entries()].filter(([, timer]) => timer.at <= end).sort((a, b) => a[1].at - b[1].at)[0]
        if (!due) break
        timers.delete(due[0])
        now = due[1].at
        due[1].fn()
      }
      now = end
    },
  }
}

test("a burst of calls runs once, after the calls stop", () => {
  const time = clock()
  let runs = 0
  const debounced = createDebounce(() => runs++, { wait: 120, maxWait: 400, timers: time })
  debounced.schedule()
  time.advance(50)
  debounced.schedule()
  time.advance(100)
  expect(runs).toBe(0)
  time.advance(30)
  expect(runs).toBe(1)
  time.advance(1000)
  expect(runs).toBe(1)
})

test("a steady stream of calls still runs at least every maxWait", () => {
  const time = clock()
  let runs = 0
  const debounced = createDebounce(() => runs++, { wait: 120, maxWait: 400, timers: time })
  for (let at = 0; at < 1000; at += 100) {
    debounced.schedule()
    time.advance(100)
  }
  // Calls every 100 ms never leave a 120 ms gap; maxWait forces a run every 400 ms.
  expect(runs).toBe(2)
  time.advance(200)
  expect(runs).toBe(3)
})

test("cancel drops a pending run", () => {
  const time = clock()
  let runs = 0
  const debounced = createDebounce(() => runs++, { wait: 120, maxWait: 400, timers: time })
  debounced.schedule()
  debounced.cancel()
  time.advance(1000)
  expect(runs).toBe(0)
})
