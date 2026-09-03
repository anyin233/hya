import { expect, test } from "bun:test"
import { testRender } from "@opentui/solid"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { ArgsProvider } from "../src/upstream/context/args"
import { ExitProvider } from "../src/upstream/context/exit"
import { KVProvider } from "../src/upstream/context/kv"
import { ProjectProvider } from "../src/upstream/context/project"
import { TuiPathsProvider, TuiStartupProvider } from "../src/upstream/context/runtime"
import { SDKProvider, type EventSource } from "../src/upstream/context/sdk"
import { SyncProvider, useSync } from "../src/upstream/context/sync"

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
