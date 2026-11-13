import { expect, test } from "bun:test"
import { createBundlesController } from "../src/app/bundles"
import { HyaClient, type FetchLike } from "../src/client"
import { createAppStore } from "../src/state/store"

for (const failure of ["", "transport", "response", "list"]) {
  test(`r refreshes before reloading and displays failures (${failure || "success"})`, async () => {
    const calls: string[] = []
    let refreshing = false
    const fetcher: FetchLike = async (input) => {
      const path = new URL(String(input)).pathname
      calls.push(path)
      if (path.endsWith(":refresh")) {
        refreshing = true
        if (failure === "transport") throw new Error("refresh unavailable")
        return Response.json({ errors: failure === "response" ? [{ bundleId: "acme/tools", message: "invalid config" }] : [] })
      }
      if (refreshing && failure === "list") throw new Error("list unavailable")
      return Response.json({ bundles: [] })
    }
    const store = createAppStore()
    const controller = createBundlesController({
      store, client: new HyaClient("http://backend", "/work", fetcher), directory: "/work",
      savePreferences: () => {}, refreshExtensions: async () => { calls.push("extensions") },
    })
    try {
      controller.open()
      // Settle the initial listing before exercising refresh.
      await Bun.sleep(0)
      calls.length = 0
      controller.key({ name: "r", sequence: "r", ctrl: false, meta: false, shift: false })
      await Bun.sleep(0)
      expect(store.state.bundlesView?.busy).toBeUndefined()
      expect(calls[0]).toBe("/v1/bundles:refresh")
      if (failure) {
        expect(store.state.bundlesView?.notice?.tone).toBe("error")
        expect(store.state.bundlesView?.notice?.text).toContain(failure === "response" ? "acme/tools: invalid config" : `${failure === "list" ? "list" : "refresh"} unavailable`)
        if (failure !== "list") expect(calls).toEqual(["/v1/bundles:refresh"])
      } else {
        expect(calls).toEqual(["/v1/bundles:refresh", "extensions", "/v1/bundles"])
        expect(store.state.bundlesView?.notice).toEqual({ tone: "ok", text: "Bundles refreshed" })
      }
    } finally { controller.dispose() }
  })
}
