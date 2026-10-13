// The model fake must track live requests, not timers or cancelled connections.
// These checks use real HTTP streams without a backend, TUI, or browser.
import { expect, test } from "./harness"
import { hangStep, startFakeModel, type FakeModel } from "./fake-model"

for (const path of ["chat/completions", "responses"]) {
  const request = (fake: FakeModel, signal: AbortSignal) => fetch(`${fake.baseUrl}/${path}`, {
    method: "POST", headers: { "content-type": "application/json" }, body: "{}", signal,
  }).then((response) => response.text())

  test.describe(`fake model lifecycle on ${path}`, () => {
    test("hang timeout removes the request from the pending queue", async () => {
      const fake = await startFakeModel([hangStep(1)])
      try {
        await request(fake, AbortSignal.timeout(10_000))
        expect(fake.pendingHangs()).toBe(0)
      } finally { await fake.stop() }
    })

    test("cancelling a hang leaves the next request releasable", async () => {
      const fake = await startFakeModel([hangStep(), hangStep()])
      const controller = new AbortController()
      const cancelled = request(fake, controller.signal).catch(() => "cancelled")
      try {
        await expect.poll(() => fake.pendingHangs()).toBe(1)
        controller.abort()
        expect(await cancelled).toBe("cancelled")
        await expect.poll(() => fake.pendingHangs()).toBe(0)

        const next = request(fake, AbortSignal.timeout(10_000))
        await expect.poll(() => fake.pendingHangs()).toBe(1)
        fake.release()
        expect(await next).toContain("data: ")
        expect(fake.pendingHangs()).toBe(0)
      } finally {
        controller.abort()
        fake.release()
        await fake.stop()
      }
    })

    test("stop settles pending hangs and closes their connections", async () => {
      const fake = await startFakeModel([hangStep()])
      const controller = new AbortController()
      const pending = request(fake, controller.signal).catch(() => "disconnected")
      let stopping: Promise<void> | undefined
      try {
        await expect.poll(() => fake.pendingHangs()).toBe(1)
        stopping = fake.stop()
        await expect.poll(() => fake.pendingHangs()).toBe(0)
        await stopping
        await pending
      } finally {
        controller.abort()
        await (stopping ?? fake.stop())
      }
    })
  })
}
