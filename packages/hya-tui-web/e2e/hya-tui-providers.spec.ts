import { readFile } from "node:fs/promises"
import { join } from "node:path"
import type { Tui } from "./harness"
import { api, backendConfigDir, expect, hangStep, hyaTui, startFakeModel, test, textStep, type Backend, type FakeModel } from "./hya"

// The Provider View (`/key`): a full-screen list of providers and, per
// provider, its models. Providers are added against a fake OpenAI-compatible
// server (`startFakeModel` with a `/v1/models` listing) that this spec starts
// itself, so the backend stays on the offline model until one is picked.

async function openProviders(term: Tui): Promise<void> {
  await term.waitForText("Enter a prompt · /new creates a session")
  await term.type("/key")
  await term.press("Enter")
  await term.waitForText("changes apply at once, no restart")
}

/** A provider `gw` on the fake, added over the API (not through the view). */
async function addOverApi(backend: Backend, fake: FakeModel, apiKey?: string): Promise<void> {
  await api(backend, "PUT", "/v1/providers/gw", { kind: "openai", baseUrl: fake.baseUrl, ...(apiKey ? { apiKey } : {}) })
}

async function withFake(steps: Parameters<typeof startFakeModel>[0], models: string[], body: (fake: FakeModel) => Promise<void>): Promise<void> {
  const fake = await startFakeModel(steps)
  fake.setModelList(models)
  try {
    await body(fake)
  } finally {
    while (fake.pendingHangs()) fake.release()
    await fake.stop()
  }
}

const config = (backend: Backend) => readFile(join(backendConfigDir(backend), "config.yaml"), "utf8")

test.describe("hya TUI Provider View", () => {
  test("/key opens the view; /keys and /login are gone; help lists the view's keys", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await term.waitForText("Connected to hya")
    await term.press("?")
    await term.waitForText("Help · keys and commands")
    await term.type("/key")
    await term.waitForText(/\/key\s+\[local\]\s+Open the Provider View/)
    await term.press("Backspace")
    await term.press("Backspace")
    await term.press("Backspace")
    await term.type("login")
    await term.waitForText("No match · Backspace widens the filter")
    for (let i = 0; i < 5; i++) await term.press("Backspace")
    await term.type("keys")
    await term.waitForText("No match · Backspace widens the filter")
    for (let i = 0; i < 5; i++) await term.press("Backspace")
    await term.type("providers")
    await term.waitForText(/t\s+\[providers\]\s+Test the highlighted model/)
    await term.press("Escape")
    await expect.poll(() => term.find("Help · keys and commands")).toBeNull()

    await openProviders(term)
    await term.waitForText(/PROVIDER\s+PROTOCOL\s+KEY\s+STATUS\s+MODELS/)
    await term.waitForText(/hya\s+offline\s+no key\s+offline\s+1 model/)
    await term.waitForText("0 configured")
    await term.waitForText("↑↓ move · Enter open · a add · k key · x remove key · r refresh · / filter · Esc close")
    // The offline provider takes no key.
    await term.type("k")
    await term.waitForText("hya is the built-in offline provider")
    await term.press("Escape")
    await term.waitForText("Enter a prompt · /new creates a session")
    expect(await term.text()).not.toContain("PROVIDER")
  })

  test("a provider added over the API while the TUI is running reaches the /model picker live, no restart or manual refresh (catalogUpdated)", async ({ tui, backend }) => {
    await withFake([], ["alpha"], async (fake) => {
      const term = await tui(hyaTui(backend))
      await term.waitForText("Connected to hya")
      // Never opens the Provider View (whose own reload would also pick this
      // up): the global stream's live `catalogUpdated` frame is what must
      // carry it to the `/model` picker.
      await addOverApi(backend, fake, "k-1")
      // The picker's rows are a snapshot taken when `/model` runs (not
      // reactive), so wait for the server-side discovery to finish, then for
      // the TUI's own debounced re-read (`catalogRefreshLater`, at most
      // 400 ms) to catch up, before opening it once — instead of racing the
      // `/model` command against that update.
      await expect.poll(async () => {
        const models = await api<{ models?: Array<{ id?: string }> }>(backend, "GET", "/v1/models")
        return (models.models ?? []).some((model) => model.id === "gw/alpha")
      }, { timeout: 20_000 }).toBe(true)
      await term.page.waitForTimeout(600)
      await term.type("/model")
      await term.press("Enter")
      await term.waitForText(/alpha\s+\[gw\]/, 5_000)
      await term.press("Escape")
    })
  })

  test("add a provider in the wizard: models are fetched, the session moves off hya/offline, a test replies", async ({ tui, backend }) => {
    await withFake([hangStep(), textStep("Hi", { finish: "length" })], ["alpha", "beta"], async (fake) => {
      const term = await tui(hyaTui(backend))
      await term.waitForText("Connected to hya")
      // A session on the offline model.
      await term.type("hello")
      await term.press("Enter")
      await term.waitForText("● build · hya/offline", 20_000)
      await openProviders(term)
      await term.type("a")
      await term.waitForText("Add provider · 1/4")
      await term.waitForText("letters, digits, - or _ (the provider id)")
      await term.type("gw")
      await term.press("Enter")
      await term.waitForText("Add provider · 2/4")
      await term.waitForText(/1\. openai\s+OpenAI-compatible Chat Completions/)
      await term.waitForText("4. google")
      await term.press("Enter")
      await term.waitForText("Add provider · 3/4")
      await term.type(fake.baseUrl)
      await term.press("Enter")
      await term.waitForText("Add provider · 4/4")
      await term.type("sk-e2e-secret")
      await term.waitForText("•".repeat("sk-e2e-secret".length))
      expect(await term.text()).not.toContain("sk-e2e-secret")
      await term.press("Enter")

      // The session runs on hya/offline: the /model picker opens over the view, the new provider's first model highlighted.
      await term.waitForText("Model · pick one of gw's models for this session")
      await term.waitForText(/▸ ● alpha\s+\[gw\]/)
      await term.press("Enter")
      await term.waitForText("Model → gw/alpha")
      await term.waitForText("Providers › gw")
      await term.waitForText(/gw · openai · http:\/\/127\.0\.0\.1:\d+\/v1 · saved key · ready · 2 models/)
      await term.waitForText(/alpha\s+remote/)
      await term.waitForText(/beta\s+remote/)
      expect(fake.modelListAuth().at(-1)).toBe("Bearer sk-e2e-secret")

      // A test that hangs shows the running line; Esc cancels it and the view stays usable.
      await term.type("t")
      await term.waitForText(/Testing gw\/alpha… \d+s · Esc cancels/)
      await term.press("Escape")
      await term.waitForText("Test cancelled")
      await term.waitForText("Providers › gw")
      fake.release()
      await term.type("t")
      await term.waitForText(/✓ gw\/alpha replied · \d+ ms · finish length · "Hi"/)
      expect(JSON.stringify(fake.requests().at(-1))).toMatch(/"max_(completion_)?tokens":1[,}]/)

      // Refresh picks up a new remote model.
      fake.setModelList(["alpha", "beta", "gamma"])
      await term.type("r")
      await term.waitForText("gw: 3 models fetched")
      await term.waitForText(/gamma\s+remote/)

      const written = await config(backend)
      expect(written).toContain("gw:")
      expect(written).toContain(`base_url: ${fake.baseUrl}`)
      expect(written).not.toContain("sk-e2e-secret")
      expect(await readFile(join(backendConfigDir(backend), "auth", "gw.yaml"), "utf8")).toContain("sk-e2e-secret")

      await term.press("Escape")
      await term.waitForText(/gw\s+openai\s+saved key\s+ready\s+3 models/)
      await term.press("Escape")
      await term.waitForText("Enter a prompt · /new creates a session")
      await term.waitForText(/build gw\/alpha/)
      // The new models reached the /model picker without a restart.
      await term.type("/model")
      await term.press("Enter")
      await term.waitForText(/gamma\s+\[gw\]/)
      await term.press("Escape")
    })
  })

  test("set, replace, and remove a provider's key", async ({ tui, backend }) => {
    await withFake([], ["alpha"], async (fake) => {
      await addOverApi(backend, fake)
      const term = await tui(hyaTui(backend))
      await openProviders(term)
      await term.waitForText(/gw\s+openai\s+no key\s+no key\s+1 model/)
      await term.type("k")
      await term.waitForText("API key · gw")
      await term.type("sk-new")
      await term.waitForText("••••••")
      expect(await term.text()).not.toContain("sk-new")
      await term.press("Enter")
      await term.waitForText("Saved the key of gw · applies now")
      await term.waitForText(/gw\s+openai\s+saved key\s+ready/)
      expect(await readFile(join(backendConfigDir(backend), "auth", "gw.yaml"), "utf8")).toContain("sk-new")

      await term.type("x")
      await term.waitForText("Remove the saved key of gw? · Enter removes · Esc cancels")
      await term.press("Escape")
      await term.waitForText("Cancelled")
      await term.type("x")
      await term.press("Enter")
      await term.waitForText("Removed the saved key of gw")
      await term.waitForText(/gw\s+openai\s+no key/)
      await term.type("x")
      await term.waitForText("gw has no saved key")
    })
  })

  test("add a model and edit a model's metadata: config / override sources, written to config.yaml", async ({ tui, backend }) => {
    await withFake([], ["alpha"], async (fake) => {
      await addOverApi(backend, fake, "k-1")
      const term = await tui(hyaTui(backend))
      await openProviders(term)
      await term.press("Enter")
      await term.waitForText("Providers › gw")
      await term.waitForText(/alpha\s+remote/)
      await term.waitForText("t test · m add model · e edit · d delete override")

      await term.type("m")
      await term.waitForText("Add model · gw · 1/5")
      await term.type("manual-1")
      await term.press("Enter")
      await term.type("Manual One")
      await term.press("Enter")
      await term.type("32000")
      await term.press("Enter")
      await term.type("4000")
      await term.press("Enter")
      await term.waitForText(/default\s+keep what the provider reports/)
      await term.press("Enter")
      await term.waitForText("Saved manual-1 to config.yaml")
      await term.waitForText(/manual-1\s+Manual One\s+config\s+32k \/ 4k/)

      // Edit the remote model (its list publishes no metadata, so the server reports the limits and
      // reasoning as unknown): only the changed field is written, and the row becomes an override.
      await term.press("ArrowUp")
      await term.type("e")
      await term.waitForText("Edit model · gw/alpha")
      await term.type("Alpha X")
      await term.press("Enter")
      await term.waitForText(/Context limit\s*▏?\s*tokens · optional/)
      await term.press("Enter")
      await term.press("Enter")
      // Reasoning: 1 default · 2 on · 3 off (a digit picks one); back on "default", as it opened: unchanged.
      await term.type("3")
      await term.waitForText("● 3. off")
      await term.type("1")
      await term.waitForText("● 1. default")
      await term.press("Enter")
      await term.waitForText("Saved alpha to config.yaml")
      await term.waitForText(/alpha\s+Alpha X\s+override\s+— \/ —/)
      expect(await term.text()).not.toMatch(/alpha\s+Alpha X.*reasoning/)
      let written = await config(backend)
      expect(written).toContain("manual-1")
      expect(written).toContain("Manual One")
      // The alpha entry holds only the new name: no fallback limit, no reasoning echoed back.
      expect(written).toMatch(/- id: alpha\n\s+name: Alpha X\n(?!\s+(limit|reasoning))/)
      expect(written).not.toContain("200000")
      expect(written).not.toContain("reasoning")

      // Clearing the name removes that field again (the entry collapses to the bare id).
      await term.type("e")
      await term.waitForText("Edit model · gw/alpha")
      await term.press("Control+u")
      for (let i = 0; i < 4; i++) await term.press("Enter")
      await term.waitForText("Saved alpha to config.yaml")
      await term.waitForText(/alpha\s+override|alpha\s+remote/)
      written = await config(backend)
      expect(written).not.toContain("Alpha X")

      // Deleting the override leaves the remote row.
      await term.type("d")
      await term.waitForText("Delete the config.yaml entry of alpha? · Enter deletes · Esc cancels")
      await term.press("Enter")
      await term.waitForText("Deleted the config.yaml entry of alpha")
      await term.waitForText(/alpha\s+remote/)
      written = await config(backend)
      expect(written).not.toMatch(/id: alpha|- alpha/)
      // A remote-only model has nothing to delete.
      await term.type("d")
      await term.waitForText("alpha comes from the remote list")
    })
  })

  test("wizard errors: Esc at every step, client-side validation, a failed fetch still adds the provider", async ({ tui, backend }) => {
    const term = await tui(hyaTui(backend))
    await openProviders(term)
    const stages: Array<() => Promise<void>> = [
      async () => {},
      async () => { await term.type("gw"); await term.press("Enter"); await term.waitForText("Add provider · 2/4") },
      async () => { await term.press("Enter"); await term.waitForText("Add provider · 3/4") },
      async () => { await term.type("http://127.0.0.1:9/v1"); await term.press("Enter"); await term.waitForText("Add provider · 4/4") },
    ]
    for (let stage = 0; stage < stages.length; stage++) {
      await term.type("a")
      await term.waitForText("Add provider · 1/4")
      for (const step of stages.slice(0, stage + 1)) await step()
      await term.press("Escape")
      await expect.poll(() => term.find("Add provider ·")).toBeNull()
      await term.waitForText("Cancelled")
    }

    await term.type("a")
    await term.type("bad id")
    await term.press("Enter")
    await term.waitForText("✗ Use 1–64 letters, digits, - or _")
    for (let i = 0; i < 6; i++) await term.press("Backspace")
    await term.type("hya")
    await term.press("Enter")
    await term.waitForText("✗ hya is reserved for the offline provider")
    for (let i = 0; i < 3; i++) await term.press("Backspace")
    await term.type("down")
    await term.press("Enter")
    await term.press("ArrowDown")
    await term.press("Enter")
    await term.type("ftp://x")
    await term.press("Enter")
    await term.waitForText("✗ Enter an http:// or https:// URL")
    for (let i = 0; i < 7; i++) await term.press("Backspace")
    await term.type("http://127.0.0.1:9/v1")
    await term.press("Enter")
    // No key: Enter skips.
    await term.press("Enter")
    await term.waitForText(/Added down · model fetch failed \(unavailable\)/)
    await term.waitForText("Providers › down")
    await term.waitForText("No models · r fetches the list · m adds one by hand")
    expect(await config(backend)).toContain("down:")
    await term.press("Escape")
    await term.waitForText(/down\s+openai-response\s+no key/)
  })

  test("at about 80 columns the list, the detail, and the wizard fit", async ({ tui, backend }) => {
    await withFake([], ["a-model-with-a-rather-long-identifier-v2", "beta"], async (fake) => {
      await addOverApi(backend, fake, "k-1")
      const term = await tui(hyaTui(backend), { viewport: { width: 690, height: 640 } })
      const { cols } = await term.size()
      expect(cols).toBeLessThanOrEqual(84)
      await openProviders(term)
      await term.waitForText(/gw\s+openai\s+saved key\s+ready\s+2 models/)
      await term.waitForText("Esc close")
      await term.press("Enter")
      await term.waitForText("Providers › gw")
      await term.waitForText(/beta\s+remote/)
      await term.waitForText(/a-model-with-a-rather…\s+remote/)
      await term.waitForText("Esc back")
      await term.type("m")
      await term.waitForText("Add model · gw · 1/5")
      await term.waitForText("Enter next · Esc cancels")
      const box = (await term.find("Add model · gw · 1/5"))!
      expect(box.col).toBeGreaterThan(0)
    })
  })
})
