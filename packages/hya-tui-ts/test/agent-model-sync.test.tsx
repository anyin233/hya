import { expect, test } from "bun:test"
import { testRender, useRenderer } from "@opentui/solid"
import { createDefaultOpenTuiKeymap } from "@opentui/keymap/opentui"
import { onCleanup, type JSX } from "solid-js"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { ClipboardProvider } from "../src/upstream/context/clipboard"
import { DialogModel } from "../src/upstream/component/dialog-model"
import { DialogProvider, useDialog } from "../src/upstream/ui/dialog"
import { ArgsProvider } from "../src/upstream/context/args"
import { ExitProvider } from "../src/upstream/context/exit"
import { KVProvider } from "../src/upstream/context/kv"
import { LocalProvider, useLocal } from "../src/upstream/context/local"
import { ProjectProvider } from "../src/upstream/context/project"
import { RouteProvider } from "../src/upstream/context/route"
import { TuiPathsProvider, TuiStartupProvider } from "../src/upstream/context/runtime"
import { SDKProvider, type EventSource } from "../src/upstream/context/sdk"
import { SyncProvider, useSync } from "../src/upstream/context/sync"
import { ThemeProvider } from "../src/upstream/context/theme"
import { ToastProvider } from "../src/upstream/ui/toast"
import { TuiConfigProvider, resolve } from "../src/upstream/config"
import { OpencodeKeymapProvider, registerOpencodeKeymap } from "../src/upstream/keymap"

const modelSelectionConfig = resolve({}, { terminalSuspend: false })

/** Provide the keymap and modal bindings required by the model dialog fixture. */
function ModelSelectionKeymapProvider(props: { children: JSX.Element }) {
  const renderer = useRenderer()
  const keymap = createDefaultOpenTuiKeymap(renderer)
  const unregister = registerOpencodeKeymap(keymap, renderer, modelSelectionConfig)
  onCleanup(unregister)
  return <OpencodeKeymapProvider keymap={keymap}>{props.children}</OpencodeKeymapProvider>
}
const provider = {
  id: "openai",
  source: "configured",
  auth: "credentialed",
  result: "models",
  models: {
    current: { source: "configured", status: "configured", variants: {} },
    next: { source: "configured", status: "configured", variants: {} },
  },
}

/** Build one normalized backend Agent model row for the sync fixture. */
function agentRow(agentID: string, modelID: string, source: "default" | "remembered") {
  return {
    agentID,
    description: `${agentID} Agent`,
    mode: agentID === "build" ? "primary" : "subagent",
    hidden: false,
    configured: false,
    settable: true,
    preference: source === "remembered" ? { providerID: "openai", modelID } : null,
    preferenceAvailable: source === "remembered",
    effective: { providerID: "openai", modelID, source },
  }
}

/** Create a response promise whose completion is controlled by the test. */
function deferredResponse() {
  let resolve!: (response: Response) => void
  const promise = new Promise<Response>((accept) => {
    resolve = accept
  })
  return { promise, resolve }
}

test("SyncProvider publishes one matching Agent row only after a successful PUT", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-agent-model-sync-")))
  const state = path.join(temp, "state")
  const project = path.join(temp, "project")
  await mkdir(state, { recursive: true })
  await mkdir(project, { recursive: true })
  await writeFile(path.join(state, "kv.json"), "{}")

  const initialBuild = agentRow("build", "current", "default")
  const initialGeneral = agentRow("general", "current", "default")
  const committedGeneral = agentRow("general", "next", "remembered")
  const delayed = deferredResponse()
  const putResponses: Array<Promise<Response> | Response> = [
    delayed.promise,
    Response.json({ error: { code: "AGENT_MODEL_CONTROL_FAILURE", message: "write failed" } }, { status: 503 }),
    Response.json(agentRow("build", "next", "remembered")),
  ]
  const requests: Array<{ method: string; pathname: string; body?: string }> = []
  const events: EventSource = {
    async subscribe() {
      return () => undefined
    },
  }
  const fetchImplementation = async (
    input: Parameters<typeof globalThis.fetch>[0],
    init?: Parameters<typeof globalThis.fetch>[1],
  ) => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url)
    const method = init?.method ?? "GET"
    requests.push({ method, pathname: url.pathname, ...(typeof init?.body === "string" ? { body: init.body } : {}) })

    if (url.pathname === "/tui/bootstrap") {
      return Response.json({
        capabilities: { agentModelPreferences: true },
        config: {},
        providers: { providers: [provider], default: {}, defaultModel: { providerID: "openai", modelID: "current" } },
        agentModels: [initialBuild, initialGeneral],
        agents: [],
        sessions: [],
        commands: [],
        lsp: [],
        mcp: {},
        mcp_resource: {},
        formatter: [],
        session_status: {},
        vcs: { branch: "main" },
      })
    }
    if (url.pathname === "/path") {
      return Response.json({ home: temp, state, config: temp, worktree: project, directory: project })
    }
    if (url.pathname === "/project/current") return Response.json({ id: "agent-model-sync", worktree: project })
    if (url.pathname.endsWith("/directories")) return Response.json([])
    if (url.pathname === "/tui/agent-models/general" && method === "PUT") {
      const response = putResponses.shift()
      if (!response) throw new Error("unexpected extra Agent model PUT")
      return response
    }
    throw new Error(`unexpected SDK request in Agent model sync test: ${method} ${url.pathname}`)
  }
  const fetch = fetchImplementation as typeof globalThis.fetch

  let sync: ReturnType<typeof useSync> | undefined
  /** Expose synchronized state through the rendered provider boundary. */
  const Probe = () => {
    sync = useSync()
    return <text>{sync.data.agentModels.map((row) => `${row.agentID}:${row.effective.modelID}`).join(" ")}</text>
  }

  const setup = await testRender(
    () => (
      <TuiPathsProvider value={{ cwd: project, home: temp, state, worktree: project }}>
        <TuiStartupProvider value={{ skipInitialLoading: true }}>
          <ExitProvider exit={(reason) => { throw reason }}>
            <ArgsProvider>
              <KVProvider>
                <SDKProvider url="http://agent-model-sync.invalid" directory={project} fetch={fetch} events={events}>
                  <ProjectProvider>
                    <SyncProvider>
                      <Probe />
                    </SyncProvider>
                  </ProjectProvider>
                </SDKProvider>
              </KVProvider>
            </ArgsProvider>
          </ExitProvider>
        </TuiStartupProvider>
      </TuiPathsProvider>
    ),
    { width: 80, height: 20, footerHeight: 0 },
  )

  try {
    for (let attempt = 0; attempt < 200 && sync?.data.status !== "complete"; attempt += 1) {
      await new Promise<void>((resolve) => setTimeout(resolve, 10))
    }
    const bound = sync
    if (!bound || bound.data.status !== "complete") {
      throw new Error(`SyncProvider did not complete bootstrap: ${JSON.stringify(requests)}`)
    }
    expect(bound.data.agentModels).toHaveLength(2)

    const pending = bound.setAgentModelPreference("general", { providerID: "openai", modelID: "next" })
    await Promise.resolve()
    expect(bound.getAgentModel("general")?.effective.modelID).toBe("current")
    expect(bound.getAgentModel("build")?.effective.modelID).toBe("current")
    expect(requests.at(-1)).toEqual({
      method: "PUT",
      pathname: "/tui/agent-models/general",
      body: JSON.stringify({ preference: { providerID: "openai", modelID: "next" } }),
    })

    delayed.resolve(Response.json(committedGeneral))
    await expect(pending).resolves.toEqual(expect.objectContaining({ agentID: "general" }))
    expect(bound.getAgentModel("general")?.effective).toEqual({
      providerID: "openai",
      modelID: "next",
      source: "remembered",
    })
    expect(bound.getAgentModel("build")?.effective.modelID).toBe("current")

    await expect(bound.setAgentModelPreference("general", null)).rejects.toThrow("write failed")
    expect(bound.getAgentModel("general")?.effective.modelID).toBe("next")

    await expect(bound.setAgentModelPreference("general", null)).rejects.toThrow(
      "returned no normalized row for general",
    )
    expect(bound.getAgentModel("general")?.effective.modelID).toBe("next")
    expect(requests.filter((request) => request.pathname === "/tui/agent-models/general")).toHaveLength(3)
  } finally {
    setup.renderer.destroy()
    await rm(temp, { recursive: true, force: true })
  }
})

test("targeted model selection updates only the active Agent request model", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-agent-model-selection-")))
  const state = path.join(temp, "state")
  const project = path.join(temp, "project")
  await mkdir(state, { recursive: true })
  await mkdir(project, { recursive: true })
  await writeFile(path.join(state, "kv.json"), "{}")

  const modelIDs = ["current", "requested", "targeted-committed", "other-committed", "normal-committed"]
  const selectionProvider = {
    id: "openai",
    source: "configured",
    auth: "credentialed",
    result: "models",
    models: Object.fromEntries(
      modelIDs.map((modelID) => [
        modelID,
        {
          source: "configured",
          status: "configured",
          variants: modelID === "targeted-committed" ? { high: {} } : {},
        },
      ]),
    ),
  }
  const initialBuild = agentRow("build", "current", "default")
  const initialGeneral = agentRow("general", "current", "default")
  const putResponses = [
    Response.json(agentRow("build", "targeted-committed", "remembered")),
    Response.json(agentRow("general", "other-committed", "remembered")),
    Response.json(agentRow("build", "normal-committed", "remembered")),
    Response.json({ error: { code: "AGENT_MODEL_CONTROL_FAILURE", message: "write failed" } }, { status: 503 }),
    Response.json(agentRow("general", "requested", "remembered")),
  ]
  const requests: Array<{ method: string; pathname: string; body?: string }> = []
  const events: EventSource = {
    async subscribe() {
      return () => undefined
    },
  }
  const fetchImplementation = async (
    input: Parameters<typeof globalThis.fetch>[0],
    init?: Parameters<typeof globalThis.fetch>[1],
  ) => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url)
    const method = init?.method ?? "GET"
    requests.push({ method, pathname: url.pathname, ...(typeof init?.body === "string" ? { body: init.body } : {}) })

    if (url.pathname === "/tui/bootstrap") {
      return Response.json({
        capabilities: { agentModelPreferences: true },
        config: {},
        providers: {
          providers: [selectionProvider],
          default: {},
          defaultModel: { providerID: "openai", modelID: "current" },
        },
        agentModels: [initialBuild, initialGeneral],
        agents: [{ name: "build", mode: "primary", hidden: false }],
        sessions: [],
        commands: [],
        lsp: [],
        mcp: {},
        mcp_resource: {},
        formatter: [],
        session_status: {},
        vcs: { branch: "main" },
      })
    }
    if (url.pathname === "/path") {
      return Response.json({ home: temp, state, config: temp, worktree: project, directory: project })
    }
    if (url.pathname === "/project/current") return Response.json({ id: "agent-model-selection", worktree: project })
    if (url.pathname.endsWith("/directories")) return Response.json([])
    if (url.pathname.startsWith("/tui/agent-models/") && method === "PUT") {
      const response = putResponses.shift()
      if (!response) throw new Error("unexpected extra Agent model PUT")
      return response
    }
    throw new Error(`unexpected SDK request in Agent model selection test: ${method} ${url.pathname}`)
  }
  const fetch = fetchImplementation as typeof globalThis.fetch

  let sync: ReturnType<typeof useSync> | undefined
  let currentModel: (() => { providerID: string; modelID: string } | undefined) | undefined
  let selectCurrent: ((model: { providerID: string; modelID: string }) => Promise<boolean>) | undefined
  let openTarget: ((agentID: string) => void) | undefined
  let seedTargetVariant: (() => string | undefined) | undefined
  let currentVariant: (() => string | undefined) | undefined
  let recentModels: (() => string[]) | undefined
  const Probe = () => {
    const boundSync = useSync()
    const local = useLocal()
    const dialog = useDialog()
    sync = boundSync
    currentModel = () => local.model.current()
    selectCurrent = (model) => local.model.select(model, { recent: true })
    openTarget = (agentID) => dialog.replace(() => <DialogModel agentID={agentID} />)
    seedTargetVariant = () => {
      local.model.set({ providerID: "openai", modelID: "targeted-committed" })
      local.model.variant.set("high")
      const seeded = local.model.variant.current()
      local.model.set({ providerID: "openai", modelID: "current" })
      return seeded
    }
    currentVariant = () => local.model.variant.current()
    recentModels = () => local.model.recent().map((model) => model.modelID)
    return <text>{local.model.current()?.modelID ?? ""}</text>
  }

  const setup = await testRender(
    () => (
      <TuiPathsProvider value={{ cwd: project, home: temp, state, worktree: project }}>
        <TuiStartupProvider value={{ skipInitialLoading: true }}>
          <ExitProvider exit={(reason) => { throw reason }}>
            <ArgsProvider>
              <KVProvider>
                <ToastProvider>
                  <TuiConfigProvider config={modelSelectionConfig}>
                    <RouteProvider>
                      <ClipboardProvider value={{}}>
                        <SDKProvider url="http://agent-model-selection.invalid" directory={project} fetch={fetch} events={events}>
                          <ProjectProvider>
                            <SyncProvider>
                              <box>
                                <text>BOOT</text>
                                <ThemeProvider mode="dark" source={{ discover: async () => ({}) }}>
                                  <LocalProvider>
                                    <ModelSelectionKeymapProvider>
                                      <DialogProvider>
                                        <Probe />
                                      </DialogProvider>
                                    </ModelSelectionKeymapProvider>
                                  </LocalProvider>
                                </ThemeProvider>
                              </box>
                            </SyncProvider>
                          </ProjectProvider>
                        </SDKProvider>
                      </ClipboardProvider>
                    </RouteProvider>
                  </TuiConfigProvider>
                </ToastProvider>
              </KVProvider>
            </ArgsProvider>
          </ExitProvider>
        </TuiStartupProvider>
      </TuiPathsProvider>
    ),
    { width: 80, height: 20, footerHeight: 0 },
  )

  try {
    for (
      let attempt = 0;
      attempt < 200 &&
      (sync?.data.status !== "complete" ||
        currentModel === undefined ||
        selectCurrent === undefined ||
        seedTargetVariant === undefined ||
        currentVariant === undefined ||
        recentModels === undefined);
      attempt += 1
    ) {
      await new Promise<void>((resolve) => setTimeout(resolve, 10))
    }
    if (
      sync?.data.status !== "complete" ||
      currentModel === undefined ||
      selectCurrent === undefined ||
      seedTargetVariant === undefined ||
      currentVariant === undefined ||
      recentModels === undefined
    ) {
      throw new Error(`Model selection fixture did not become ready: ${JSON.stringify(setup.captureCharFrame())}`)
    }
    expect(seedTargetVariant()).toBe("high")
    openTarget!("build")
    await setup.flush()
    expect(setup.captureCharFrame()).toContain("Select model for build")
    await setup.mockInput.typeText("requested")
    setup.mockInput.pressEnter()
    await setup.waitFor(() => requests.filter((request) => request.method === "PUT").length === 1)
    expect(requests.filter((request) => request.method === "PUT")[0]).toEqual({
      method: "PUT",
      pathname: "/tui/agent-models/build",
      body: JSON.stringify({ preference: { providerID: "openai", modelID: "requested" } }),
    })
    await setup.waitFor(() => currentModel!()?.modelID === "targeted-committed")
    expect(sync!.getAgentModel("build")?.effective.modelID).toBe("targeted-committed")
    expect(currentVariant()).toBeUndefined()

    openTarget!("general")
    await setup.flush()
    expect(setup.captureCharFrame()).toContain("Select model for general")
    await setup.mockInput.typeText("requested")
    setup.mockInput.pressEnter()
    await setup.waitFor(() => requests.filter((request) => request.method === "PUT").length === 2)
    expect(requests.filter((request) => request.method === "PUT")[1]).toEqual({
      method: "PUT",
      pathname: "/tui/agent-models/general",
      body: JSON.stringify({ preference: { providerID: "openai", modelID: "requested" } }),
    })
    await setup.waitFor(() => sync!.getAgentModel("general")?.effective.modelID === "other-committed")
    expect(currentModel!()?.modelID).toBe("targeted-committed")

    await expect(selectCurrent!({ providerID: "openai", modelID: "requested" })).resolves.toBe(true)
    await setup.waitFor(() => requests.filter((request) => request.method === "PUT").length === 3)
    expect(requests.filter((request) => request.method === "PUT")[2]).toEqual({
      method: "PUT",
      pathname: "/tui/agent-models/build",
      body: JSON.stringify({ preference: { providerID: "openai", modelID: "requested" } }),
    })
    await setup.waitFor(() => currentModel!()?.modelID === "normal-committed")
    expect(sync!.getAgentModel("build")?.effective.modelID).toBe("normal-committed")

    const recentBeforeFailures = recentModels()
    await expect(selectCurrent({ providerID: "openai", modelID: "requested" })).resolves.toBe(false)
    expect(currentModel()?.modelID).toBe("normal-committed")
    expect(recentModels()).toEqual(recentBeforeFailures)
    expect(sync.getAgentModel("build")?.effective.modelID).toBe("normal-committed")

    await expect(selectCurrent({ providerID: "openai", modelID: "requested" })).resolves.toBe(false)
    expect(currentModel()?.modelID).toBe("normal-committed")
    expect(recentModels()).toEqual(recentBeforeFailures)
    expect(sync.getAgentModel("build")?.effective.modelID).toBe("normal-committed")
    expect(requests.filter((request) => request.method === "PUT")).toHaveLength(5)
  } finally {
    setup.renderer.destroy()
    await rm(temp, { recursive: true, force: true })
  }
})
