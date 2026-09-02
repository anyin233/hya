import { expect, test } from "bun:test"
import type { Message, Session, ToolPart } from "@opencode-ai/sdk/v2"
import type { GlobalEvent } from "@opencode-ai/sdk/v2/client"
import { testRender } from "@opentui/solid"
import { createEffect, createMemo, onMount, Show } from "solid-js"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { CodingToolPresentation } from "../src/hya/coding-tool-presentation"
import { ArgsProvider } from "../src/upstream/context/args"
import { ExitProvider } from "../src/upstream/context/exit"
import { KVProvider } from "../src/upstream/context/kv"
import { ProjectProvider } from "../src/upstream/context/project"
import { TuiPathsProvider, TuiStartupProvider } from "../src/upstream/context/runtime"
import { SDKProvider, type EventSource } from "../src/upstream/context/sdk"
import { SyncProvider, useSync } from "../src/upstream/context/sync"

/** Build one completed SDK tool part with valid coding-tool display metadata. */
function completedPart(output: string, text: string, lineStart = 1): ToolPart {
  return {
    id: "part-sync-coding-tool",
    sessionID: "session-sync-coding-tool",
    messageID: "message-sync-coding-tool",
    type: "tool",
    callID: "call-sync-coding-tool",
    tool: "read",
    state: {
      status: "completed",
      input: { path: "src/sync.ts" },
      output,
      title: "Read src/sync.ts",
      metadata: {
        display: {
          type: "file",
          path: "src/sync.ts",
          text,
          lineStart,
          truncated: false,
        },
      },
      time: { start: 1, end: 2 },
    },
  }
}

/** Flatten captured terminal spans into text while preserving frame line boundaries. */
function frameText(frame: { lines: Array<{ spans: Array<{ text: string }> }> }): string {
  return frame.lines.map((line) => line.spans.map((span) => span.text).join("")).join("\n")
}

test("SyncProvider hydrates one coding-tool part and replaces it from one event without a second presentation owner", async () => {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-coding-tool-sync-")))
  const state = path.join(temp, "state")
  const project = path.join(temp, "project")
  await mkdir(state, { recursive: true })
  await mkdir(project, { recursive: true })
  await writeFile(path.join(state, "kv.json"), "{}")

  const sessionID = "session-sync-coding-tool"
  const messageID = "message-sync-coding-tool"
  const partID = "part-sync-coding-tool"
  const initialPart = completedPart("HYDRATED_OUTPUT", "HYDRATED_CONTENT")
  const replacementPart = completedPart("REPLACED_OUTPUT", "REPLACED_CONTENT", 4)
  const session = {
    id: sessionID,
    slug: "sync-coding-tool",
    projectID: "project-sync-coding-tool",
    directory: project,
    title: "Sync coding-tool test",
    version: "test",
    time: { created: 1, updated: 2 },
  } satisfies Session
  const message = {
    id: messageID,
    sessionID,
    role: "assistant",
    time: { created: 1, completed: 2 },
    parentID: "parent-sync-coding-tool",
    modelID: "model-sync-coding-tool",
    providerID: "provider-sync-coding-tool",
    mode: "build",
    agent: "build",
    path: { cwd: project, root: project },
    cost: 0,
    tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
  } as Message

  const requests: string[] = []
  let eventHandler: ((event: GlobalEvent) => void) | undefined
  let eventSubscriptions = 0
  const events: EventSource = {
    async subscribe(handler) {
      eventSubscriptions += 1
      eventHandler = handler
      return () => {
        if (eventHandler === handler) eventHandler = undefined
      }
    },
  }
  const fetchImplementation = async (input: Parameters<typeof globalThis.fetch>[0]) => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url)
    requests.push(url.pathname)

    if (url.pathname === "/tui/bootstrap") {
      return Response.json({
        config: {},
        providers: { providers: [], default: {} },
        agents: [],
        sessions: [session],
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
    if (url.pathname === "/project/current") return Response.json({ id: session.projectID, worktree: project })
    if (url.pathname.endsWith("/directories")) return Response.json([])
    if (url.pathname === `/session/${sessionID}`) return Response.json(session)
    if (url.pathname === `/session/${sessionID}/message`) {
      return Response.json([{ info: message, parts: [initialPart] }])
    }
    if (url.pathname === `/session/${sessionID}/todo`) return Response.json([])
    if (url.pathname === `/session/${sessionID}/diff`) return Response.json([])
    throw new Error(`unexpected SDK request in sync test: ${url.pathname}`)
  }
  const fetch = fetchImplementation as typeof globalThis.fetch

  const renderedOutputs: string[] = []
  let hydration: Promise<void> | undefined
  const Probe = () => {
    const sync = useSync()
    const current = createMemo(() => sync.data.part[messageID]?.find((part) => part.id === partID) as ToolPart | undefined)

    createEffect(() => {
      const part = current()
      if (part?.state.status === "completed") renderedOutputs.push(part.state.output)
    })
    onMount(() => {
      hydration = sync.session.sync(sessionID)
    })

    return (
      <Show when={current()}>
        {(part) => <CodingToolPresentation part={part()} width={80} diffStyle="auto" diffWrapMode="none" />}
      </Show>
    )
  }

  const setup = await testRender(
    () => (
      <TuiPathsProvider value={{ cwd: project, home: temp, state, worktree: project }}>
        <TuiStartupProvider value={{ skipInitialLoading: true }}>
          <ExitProvider exit={(reason) => { throw reason }}>
            <ArgsProvider>
              <KVProvider>
                <SDKProvider url="http://sync-test.invalid" directory={project} fetch={fetch} events={events}>
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
    for (let attempt = 0; attempt < 200 && hydration === undefined; attempt += 1) {
      await new Promise<void>((resolve) => setTimeout(resolve, 10))
    }
    const hydrationPromise = hydration
    if (!hydrationPromise) throw new Error("SyncProvider did not start session hydration")
    await hydrationPromise
    await setup.waitForFrame((frame) => frame.includes("HYDRATED_CONTENT"))
    expect(renderedOutputs).toEqual(["HYDRATED_OUTPUT"])
    expect(eventSubscriptions).toBe(1)
    expect(requests.filter((request) => request.startsWith(`/session/${sessionID}`))).toHaveLength(4)
    expect(requests.some((request) => request.includes("/tool"))).toBe(false)
    await setup.waitFor(() => eventHandler !== undefined)

    const requestsBeforeReplacement = requests.length
    let presentationTimers = 0
    const originalSetTimeout = globalThis.setTimeout
    const originalSetInterval = globalThis.setInterval
    globalThis.setTimeout = ((...args: Parameters<typeof setTimeout>) => {
      presentationTimers += 1
      return originalSetTimeout(...args)
    }) as typeof setTimeout
    globalThis.setInterval = ((...args: Parameters<typeof setInterval>) => {
      presentationTimers += 1
      return originalSetInterval(...args)
    }) as typeof setInterval
    try {
      eventHandler!({
        directory: project,
        payload: {
          id: "event-sync-coding-tool-replacement",
          type: "message.part.updated",
          properties: { sessionID, part: replacementPart, time: 3 },
        },
      })
    } finally {
      globalThis.setTimeout = originalSetTimeout
      globalThis.setInterval = originalSetInterval
    }

    await setup.waitForFrame((frame) => frame.includes("REPLACED_CONTENT"))
    expect(frameText(setup.captureSpans())).toContain("REPLACED_CONTENT")
    expect(renderedOutputs).toEqual(["HYDRATED_OUTPUT", "REPLACED_OUTPUT"])
    expect(requests).toHaveLength(requestsBeforeReplacement)
    expect(presentationTimers).toBe(0)
  } finally {
    setup.renderer.destroy()
    await rm(temp, { recursive: true, force: true })
  }
})
