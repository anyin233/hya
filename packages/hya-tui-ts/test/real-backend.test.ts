import { afterEach, expect, test } from "bun:test"
import { createOpencodeClient, type GlobalEvent } from "@opencode-ai/sdk/v2/client"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

const root = path.resolve(import.meta.dir, "../../..")
const backend = path.join(root, "target/debug/hya-backend")
const cleanups: Array<() => Promise<void>> = []

afterEach(async () => {
  await Promise.all(cleanups.splice(0).map((cleanup) => cleanup()))
})

type StartBackendOptions = {
  yolo?: boolean
  providerUrl?: string
  providerModels?: readonly string[]
  model?: string
}

async function startBackend({
  yolo = true,
  providerUrl,
  providerModels = ["model"],
  model = "fixture/model",
}: StartBackendOptions = {}) {
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-real-backend-")))
  const project = path.join(temp, "project")
  await mkdir(project)
  await writeFile(path.join(project, "README.md"), "real backend fixture\n")
  const configFile = path.join(temp, "config", "hya", "config.yaml")
  /** Replace the isolated fixture catalog without changing its Session database. */
  const setProviderModels = async (models: readonly string[]) => {
    if (!providerUrl) throw new Error("provider catalog is unavailable without a provider URL")
    await mkdir(path.dirname(configFile), { recursive: true })
    await writeFile(
      configFile,
      `default_model: ${JSON.stringify(model)}
providers:
  fixture:
    kind: openai
    base_url: ${providerUrl}/v1
    api_key: test
    models: [${models.map((entry) => JSON.stringify(entry)).join(", ")}]
mcp: {}
plugins: {}
`,
    )
  }
  if (providerUrl) await setProviderModels(providerModels)
  const args = [
    backend,
    ...(yolo ? ["--yolo"] : []),
    ...(providerUrl ? ["--model", model] : []),
    "--db",
    path.join(temp, "sessions.db"),
    "serve",
    "--bind",
    "127.0.0.1:0",
  ]
  const env = {
    ...Bun.env,
    HOME: path.join(temp, "home"),
    XDG_CONFIG_HOME: path.join(temp, "config"),
    XDG_STATE_HOME: path.join(temp, "state"),
    XDG_CACHE_HOME: path.join(temp, "cache"),
  }
  let serverProcess: Bun.Subprocess | undefined
  const stop = async () => {
    const current = serverProcess
    serverProcess = undefined
    if (!current) return
    current.kill()
    await Promise.race([current.exited, Bun.sleep(2_000).then(() => current.kill(9))])
  }
  const start = async () => {
    const current = Bun.spawn(args, {
      cwd: project,
      env,
      stdout: "pipe",
      stderr: "pipe",
    })
    serverProcess = current
    const reader = current.stdout.getReader()
    const decoder = new TextDecoder()
    let output = ""
    return Promise.race([
      (async () => {
        while (true) {
          const chunk = await reader.read()
          if (chunk.done) throw new Error(`hya-backend exited before readiness: ${output}`)
          output += decoder.decode(chunk.value, { stream: true })
          const match = output.match(/hya server listening on (http:\/\/127\.0\.0\.1:\d+)/)
          if (match) return match[1]
        }
      })(),
      Bun.sleep(10_000).then(() => {
        throw new Error(`timed out waiting for hya-backend: ${output}`)
      }),
    ])
  }
  const url = await start()
  cleanups.push(async () => {
    await stop()
    await rm(temp, { recursive: true, force: true })
  })
  const restart = async () => {
    await stop()
    return start()
  }
  return { project, url, restart, setProviderModels }
}

test("pinned SDK resolves real shell permissions exactly once", async () => {
  const { project, url } = await startBackend({ yolo: false })
  const client = createOpencodeClient({ baseUrl: url, directory: project })
  const events: GlobalEvent[] = []
  const eventAbort = new AbortController()
  const stream = await client.global.event({ signal: eventAbort.signal, sseMaxRetryAttempts: 0 })
  const eventTask = (async () => {
    for await (const event of stream.stream) events.push(event)
  })()
  cleanups.push(async () => {
    eventAbort.abort()
    await eventTask.catch(() => {})
  })
  await waitFor(() => events.some((event) => event.payload.type === "server.connected"), "SDK-decoded server.connected")

  const created = await client.session.create({ title: "Permission lifecycle" }, { throwOnError: true })
  const sessionID = created.data!.id
  const allowCommand = "printf allowed > permission-once.txt"
  const allowed = client.session.shell({ sessionID, command: allowCommand }, { throwOnError: true })
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "permission.asked" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.patterns.includes(allowCommand),
      ),
    "permission.asked",
  )
  const asked = events.find(
    (event) => event.payload.type === "permission.asked" && event.payload.properties.sessionID === sessionID,
  )!
  if (asked.payload.type !== "permission.asked") throw new Error("expected permission.asked")

  const listed = (await client.permission.list({}, { throwOnError: true })).data!
  await client.permission.reply({ requestID: listed[0].id, reply: "once" }, { throwOnError: true })
  await allowed
  expect(asked.directory).toBe(project)
  expect(asked.payload.properties.always).toEqual([allowCommand])
  expect(listed).toHaveLength(1)
  expect(listed[0]).toMatchObject({
    id: asked.payload.properties.id,
    sessionID,
    permission: "bash",
    patterns: [allowCommand],
    metadata: {},
    always: [allowCommand],
  })
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "permission.replied" &&
          event.payload.properties.requestID === listed[0].id &&
          event.payload.properties.reply === "once",
      ),
    "permission.replied once",
  )
  expect(await Bun.file(path.join(project, "permission-once.txt")).text()).toBe("allowed")
  expect(
    events.filter(
      (event) => event.payload.type === "permission.replied" && event.payload.properties.requestID === listed[0].id,
    ),
  ).toHaveLength(1)
  const duplicateOnce = await client.permission.reply({ requestID: listed[0].id, reply: "once" })
  expect(duplicateOnce.response.status).toBe(404)

  const rejectCommand = "printf rejected > permission-reject.txt"
  const rejected = client.session.shell({ sessionID, command: rejectCommand }, { throwOnError: true })
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "permission.asked" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.patterns.includes(rejectCommand),
      ),
    "second permission.asked",
  )
  const rejectAsked = events.find(
    (event) => event.payload.type === "permission.asked" && event.payload.properties.patterns.includes(rejectCommand),
  )!
  if (rejectAsked.payload.type !== "permission.asked") throw new Error("expected second permission.asked")
  const rejectRequestID = rejectAsked.payload.properties.id
  await client.permission.reply({ requestID: rejectRequestID, reply: "reject" }, { throwOnError: true })
  await rejected
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "permission.replied" &&
          event.payload.properties.requestID === rejectRequestID &&
          event.payload.properties.reply === "reject",
      ),
    "permission.replied reject",
  )
  expect(await Bun.file(path.join(project, "permission-reject.txt")).exists()).toBe(false)
  expect(
    events.filter(
      (event) =>
        event.payload.type === "permission.replied" &&
        event.payload.properties.requestID === rejectRequestID,
    ),
  ).toHaveLength(1)
  const duplicateReject = await client.permission.reply({
    requestID: rejectRequestID,
    reply: "reject",
  })
  expect(duplicateReject.response.status).toBe(404)
  expect((await client.permission.list({}, { throwOnError: true })).data).toEqual([])
}, 30_000)

test("pinned SDK resolves real question replies and rejections exactly once", async () => {
  const providerRequests: Array<{ messages?: Array<{ role?: string }> }> = []
  const provider = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      if (new URL(request.url).pathname !== "/v1/chat/completions") return new Response("not found", { status: 404 })
      const body = (await request.json()) as { messages?: Array<{ role?: string }> }
      providerRequests.push(body)
      const latest = body.messages?.at(-1)
      const chunks =
        latest?.role === "tool"
          ? [
              { choices: [{ delta: { content: "question lifecycle complete" }, finish_reason: null }] },
              { choices: [{ delta: {}, finish_reason: "stop" }] },
            ]
          : [
              {
                choices: [
                  {
                    delta: {
                      tool_calls: [
                        {
                          index: 0,
                          id: `call_${providerRequests.length}`,
                          type: "function",
                          function: {
                            name: "question",
                            arguments: JSON.stringify({
                              questions: [
                                {
                                  question: JSON.stringify(latest).includes("reject lifecycle")
                                    ? "Reject this question?"
                                    : "Reply to this question?",
                                  header: "Lifecycle",
                                  options: [{ label: "Approve", description: "Exercise reply" }],
                                  multiple: false,
                                  custom: false,
                                },
                              ],
                            }),
                          },
                        },
                      ],
                    },
                    finish_reason: null,
                  },
                ],
              },
              { choices: [{ delta: {}, finish_reason: "tool_calls" }] },
            ]
      return new Response(`${chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("")}data: [DONE]\n\n`, {
        headers: { "content-type": "text/event-stream" },
      })
    },
  })
  cleanups.push(async () => provider.stop(true))

  const { project, url } = await startBackend({ yolo: false, providerUrl: `http://127.0.0.1:${provider.port}` })
  const client = createOpencodeClient({ baseUrl: url, directory: project })
  const events: GlobalEvent[] = []
  const eventAbort = new AbortController()
  const stream = await client.global.event({ signal: eventAbort.signal, sseMaxRetryAttempts: 0 })
  const eventTask = (async () => {
    for await (const event of stream.stream) events.push(event)
  })()
  cleanups.push(async () => {
    eventAbort.abort()
    await eventTask.catch(() => {})
  })
  await waitFor(() => events.some((event) => event.payload.type === "server.connected"), "SDK-decoded server.connected")

  const created = await client.session.create({ title: "Question lifecycle" }, { throwOnError: true })
  const sessionID = created.data!.id
  const approveQuestionTool = async () => {
    await waitFor(
      async () => (await client.permission.list({}, { throwOnError: true })).data!.length === 1,
      "question tool permission",
    )
    const [asked] = (await client.permission.list({}, { throwOnError: true })).data!
    expect(asked).toMatchObject({ sessionID, permission: "tool", patterns: ["question"] })
    await client.permission.reply(
      { requestID: asked.id, reply: "once" },
      { throwOnError: true },
    )
  }
  await client.session.promptAsync(
    { sessionID, parts: [{ type: "text", text: "reply lifecycle" }] },
    { throwOnError: true },
  )
  await approveQuestionTool()
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "question.asked" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.questions[0]?.question === "Reply to this question?",
      ),
    "question.asked for reply",
  )
  const replyAsked = events.find(
    (event) =>
      event.payload.type === "question.asked" &&
      event.payload.properties.questions[0]?.question === "Reply to this question?",
  )!
  if (replyAsked.payload.type !== "question.asked") throw new Error("expected reply question.asked")
  const replyRequestID = replyAsked.payload.properties.id
  expect(replyAsked.directory).toBe(project)
  expect((await client.question.list({}, { throwOnError: true })).data).toEqual([replyAsked.payload.properties])

  await client.question.reply(
    { requestID: replyRequestID, answers: [["Approve"]] },
    { throwOnError: true },
  )
  await waitFor(
    () => providerRequests.filter((body) => body.messages?.at(-1)?.role === "tool").length === 1,
    "question reply tool round",
  )
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "session.status" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.status.type === "idle",
      ),
    "idle session event after question reply",
  )
  const replied = events.find(
    (event) =>
      event.payload.type === "question.replied" && event.payload.properties.requestID === replyRequestID,
  )!
  if (replied.payload.type !== "question.replied") throw new Error("expected question.replied")
  expect(replied.payload.properties.answers).toEqual([["Approve"]])
  expect(
    events.filter(
      (event) =>
        event.payload.type === "question.replied" && event.payload.properties.requestID === replyRequestID,
    ),
  ).toHaveLength(1)
  const duplicateReply = await client.question.reply({
    requestID: replyRequestID,
    answers: [["Approve"]],
  })
  expect(duplicateReply.response.status).toBe(404)
  expect((await client.question.list({}, { throwOnError: true })).data).toEqual([])

  await client.session.promptAsync(
    { sessionID, parts: [{ type: "text", text: "reject lifecycle" }] },
    { throwOnError: true },
  )
  await approveQuestionTool()
  await waitFor(
    () =>
      events.some(
        (event) =>
          event.payload.type === "question.asked" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.questions[0]?.question === "Reject this question?",
      ),
    "question.asked for reject",
  )
  const rejectAsked = events.find(
    (event) =>
      event.payload.type === "question.asked" &&
      event.payload.properties.questions[0]?.question === "Reject this question?",
  )!
  if (rejectAsked.payload.type !== "question.asked") throw new Error("expected reject question.asked")
  const rejectRequestID = rejectAsked.payload.properties.id
  expect((await client.question.list({}, { throwOnError: true })).data).toEqual([rejectAsked.payload.properties])
  await client.question.reject({ requestID: rejectRequestID }, { throwOnError: true })
  await waitFor(
    () => providerRequests.filter((body) => body.messages?.at(-1)?.role === "tool").length === 2,
    "question reject tool round",
  )
  await waitFor(
    () =>
      events.filter(
        (event) =>
          event.payload.type === "session.status" &&
          event.payload.properties.sessionID === sessionID &&
          event.payload.properties.status.type === "idle",
      ).length === 2,
    "second idle session event after question reject",
  )
  expect(
    events.filter(
      (event) =>
        event.payload.type === "question.rejected" && event.payload.properties.requestID === rejectRequestID,
    ),
  ).toHaveLength(1)
  const duplicateReject = await client.question.reject({ requestID: rejectRequestID })
  expect(duplicateReject.response.status).toBe(404)
  expect((await client.question.list({}, { throwOnError: true })).data).toEqual([])
  expect(providerRequests.map((body) => body.messages?.at(-1)?.role)).toEqual(["user", "tool", "user", "tool"])
}, 30_000)

async function waitFor(check: () => boolean | Promise<boolean>, message: string) {
  const deadline = Date.now() + 5_000
  while (!(await check())) {
    if (Date.now() >= deadline) throw new Error(`timed out waiting for ${message}`)
    await Bun.sleep(20)
  }
}

test("pinned SDK synchronous prompt publishes busy then idle status", async () => {
  const { project, url } = await startBackend()
  const client = createOpencodeClient({ baseUrl: url, directory: project })
  const events: GlobalEvent[] = []
  const eventAbort = new AbortController()
  const stream = await client.global.event({ signal: eventAbort.signal, sseMaxRetryAttempts: 0 })
  const eventTask = (async () => {
    for await (const event of stream.stream) events.push(event)
  })()
  cleanups.push(async () => {
    eventAbort.abort()
    await eventTask.catch(() => {})
  })
  await waitFor(() => events.some((event) => event.payload.type === "server.connected"), "SDK-decoded server.connected")

  const created = await client.session.create({ title: "Synchronous prompt status" }, { throwOnError: true })
  const sessionID = created.data!.id
  const statuses = () =>
    events.flatMap((event) =>
      event.payload.type === "session.status" && event.payload.properties.sessionID === sessionID
        ? [event.payload.properties.status.type]
        : [],
    )

  const prompt = client.session.prompt(
    { sessionID, parts: [{ type: "text", text: "synchronous status lifecycle" }] },
    { throwOnError: true },
  )
  await waitFor(() => statuses().includes("busy"), "busy session event")
  await prompt
  await waitFor(() => statuses().includes("idle"), "idle session event")
  expect(statuses()).toEqual(["busy", "idle"])
}, 30_000)

test("pinned SDK drives the retained TUI workflow against a real hya backend", async () => {
  const { project, url } = await startBackend()
  const client = createOpencodeClient({ baseUrl: url, directory: project })
  const events: GlobalEvent[] = []
  const eventAbort = new AbortController()
  const stream = await client.global.event({ signal: eventAbort.signal, sseMaxRetryAttempts: 0 })
  const eventTask = (async () => {
    for await (const event of stream.stream) events.push(event)
  })()
  cleanups.push(async () => {
    eventAbort.abort()
    await eventTask.catch(() => {})
  })
  await waitFor(() => JSON.stringify(events).includes("server.connected"), "SDK-decoded server.connected")

  const currentProject = await client.project.current({}, { throwOnError: true })
  const projectID = currentProject.data!.id
  const bootstrap = await Promise.all([
    client.path.get({}, { throwOnError: true }),
    client.project.directories({ projectID }, { throwOnError: true }),
    client.config.get({}, { throwOnError: true }),
    client.config.providers({}, { throwOnError: true }),
    client.provider.list({}, { throwOnError: true }),
    client.v2.model.list({}, { throwOnError: true }),
    client.app.agents({}, { throwOnError: true }),
    client.command.list({}, { throwOnError: true }),
    client.session.list({}, { throwOnError: true }),
    client.session.status({}, { throwOnError: true }),
    client.file.list({ path: "." }, { throwOnError: true }),
    client.file.read({ path: "README.md" }, { throwOnError: true }),
    client.file.status({}, { throwOnError: true }),
    client.find.files({ query: "README" }, { throwOnError: true }),
    client.mcp.status({}, { throwOnError: true }),
    client.lsp.status({}, { throwOnError: true }),
    client.formatter.status({}, { throwOnError: true }),
  ])
  expect(bootstrap[0].data!.directory).toBe(project)
  expect(bootstrap[11].data!.content).toContain("real backend fixture")

  const created = await client.session.create({ title: "SDK workflow" }, { throwOnError: true })
  const sessionID = created.data!.id
  expect((await client.session.get({ sessionID }, { throwOnError: true })).data!.title).toBe("SDK workflow")
  expect((await client.session.list({}, { throwOnError: true })).data!.some((session) => session.id === sessionID)).toBe(true)
  expect((await client.session.update({ sessionID, title: "Renamed by SDK" }, { throwOnError: true })).data!.title).toBe(
    "Renamed by SDK",
  )

  await client.session.promptAsync(
    { sessionID, parts: [{ type: "text", text: "deterministic SDK prompt" }] },
    { throwOnError: true },
  )
  await waitFor(() => {
    const streamed = JSON.stringify(events)
    return streamed.includes("deterministic SDK prompt") && streamed.includes("No live provider is available")
  }, "streamed assistant text")
  await waitFor(
    async () => (await client.session.status({}, { throwOnError: true })).data![sessionID]?.type !== "busy",
    "completed prompt",
  )

  await client.session.shell({ sessionID, command: "printf sdk-tool-activity" }, { throwOnError: true })
  await waitFor(
    () => JSON.stringify(events).includes("message.part.updated") && JSON.stringify(events).includes("sdk-tool-activity"),
    "streamed shell tool activity",
  )

  const runningShell = client.session.shell({ sessionID, command: "sleep 20" }, { throwOnError: true })
  await waitFor(async () => (await client.session.status({}, { throwOnError: true })).data![sessionID]?.type === "busy", "running shell")
  await client.session.abort({ sessionID }, { throwOnError: true })
  await runningShell

  expect((await client.session.messages({ sessionID }, { throwOnError: true })).data!.length).toBeGreaterThan(0)
  expect((await client.session.todo({ sessionID }, { throwOnError: true })).data).toEqual([])
  expect((await client.session.diff({ sessionID }, { throwOnError: true })).data).toEqual([])
  expect((await client.permission.list({}, { throwOnError: true })).data).toEqual([])
  expect((await client.question.list({}, { throwOnError: true })).data).toEqual([])
  expect((await client.session.delete({ sessionID }, { throwOnError: true })).data).toBe(true)
  expect((await client.session.list({}, { throwOnError: true })).data!.some((session) => session.id === sessionID)).toBe(false)
}, 30_000)
test("real backend uses targeted Agent model preference through restart", async () => {
  const providerModels: unknown[] = []
  const provider = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      if (new URL(request.url).pathname !== "/v1/chat/completions") {
        return new Response("not found", { status: 404 })
      }
      const body = (await request.json()) as { model?: unknown }
      providerModels.push(body.model)
      const chunks = [
        { choices: [{ delta: { content: "fixture response" }, finish_reason: null }] },
        { choices: [{ delta: {}, finish_reason: "stop" }] },
      ]
      return new Response(
        `${chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("")}data: [DONE]\n\n`,
        { headers: { "content-type": "text/event-stream" } },
      )
    },
  })
  cleanups.push(async () => provider.stop(true))

  const { project, url, restart, setProviderModels } = await startBackend({
    providerUrl: `http://127.0.0.1:${provider.port}`,
    providerModels: ["default", "selected"],
    model: "fixture/default",
  })
  const headers = {
    "x-opencode-directory": project,
    accept: "application/json",
  }
  const listResponse = await fetch(`${url}/tui/agent-models`, { headers })
  if (!listResponse.ok) {
    throw new Error(`GET /tui/agent-models failed with status ${listResponse.status}`)
  }
  type AgentModelRow = {
    agentID: string
    mode: string
    configured: boolean
    settable: boolean
    preference: { providerID: string; modelID: string } | null
    preferenceAvailable: boolean
    effective: { providerID: string; modelID: string; source: string }
  }
  const rows = (await listResponse.json()) as AgentModelRow[]
  const eligible = rows
    .filter((row) => row.mode === "primary" && !row.configured && row.settable)
    .sort((left, right) => (left.agentID < right.agentID ? -1 : left.agentID > right.agentID ? 1 : 0))
  if (eligible.length < 2) {
    throw new Error(
      `GET /tui/agent-models exposed ${eligible.length} settable unconfigured primary Agents; expected at least two`,
    )
  }
  const agentA = eligible[0]
  const agentB = eligible[1]
  if (!agentA || !agentB) throw new Error("GET /tui/agent-models did not expose Agent A and Agent B")
  expect(agentA.effective).toEqual({ providerID: "fixture", modelID: "default", source: "default" })
  expect(agentB.effective).toEqual({ providerID: "fixture", modelID: "default", source: "default" })

  const selectionResponse = await fetch(`${url}/tui/agent-models/${encodeURIComponent(agentB.agentID)}`, {
    method: "PUT",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify({ preference: { providerID: "fixture", modelID: "selected" } }),
  })
  if (!selectionResponse.ok) {
    throw new Error(`PUT /tui/agent-models/${agentB.agentID} failed with status ${selectionResponse.status}`)
  }
  const selected = (await selectionResponse.json()) as AgentModelRow
  expect(selected).toMatchObject({
    agentID: agentB.agentID,
    configured: false,
    settable: true,
    preference: { providerID: "fixture", modelID: "selected" },
    preferenceAvailable: true,
    effective: { providerID: "fixture", modelID: "selected", source: "remembered" },
  })

  const afterSelectionResponse = await fetch(`${url}/tui/agent-models`, { headers })
  if (!afterSelectionResponse.ok) {
    throw new Error(`GET /tui/agent-models after PUT failed with status ${afterSelectionResponse.status}`)
  }
  const afterSelection = (await afterSelectionResponse.json()) as AgentModelRow[]
  expect(afterSelection.find((row) => row.agentID === agentA.agentID)?.effective).toEqual({
    providerID: "fixture",
    modelID: "default",
    source: "default",
  })

  const client = createOpencodeClient({ baseUrl: url, directory: project })
  const bSession = (
    await client.session.create({ title: "Agent B selected model", agent: agentB.agentID }, { throwOnError: true })
  ).data!
  // The child process and real HTTP provider cannot use deterministic fake timers; this bounds completion instead.
  await Promise.race([
    client.session.prompt(
      { sessionID: bSession.id, parts: [{ type: "text", text: "prompt Agent B" }] },
      { throwOnError: true },
    ),
    Bun.sleep(5_000).then(() => {
      throw new Error("timed out waiting for Agent B provider completion")
    }),
  ])
  expect(providerModels).toEqual(["selected"])

  const aSession = (
    await client.session.create({ title: "Agent A default model", agent: agentA.agentID }, { throwOnError: true })
  ).data!
  await Promise.race([
    client.session.prompt(
      { sessionID: aSession.id, parts: [{ type: "text", text: "prompt Agent A" }] },
      { throwOnError: true },
    ),
    Bun.sleep(5_000).then(() => {
      throw new Error("timed out waiting for Agent A provider completion")
    }),
  ])
  expect(providerModels).toEqual(["selected", "default"])

  const restartedUrl = await restart()
  const restartedClient = createOpencodeClient({ baseUrl: restartedUrl, directory: project })
  const restartedSession = (
    await restartedClient.session.create(
      { title: "Agent B selected model after restart", agent: agentB.agentID },
      { throwOnError: true },
    )
  ).data!
  await Promise.race([
    restartedClient.session.prompt(
      { sessionID: restartedSession.id, parts: [{ type: "text", text: "prompt Agent B after restart" }] },
      { throwOnError: true },
    ),
    Bun.sleep(5_000).then(() => {
      throw new Error("timed out waiting for restarted Agent B provider completion")
    }),
  ])
  expect(providerModels).toEqual(["selected", "default", "selected"])

  const clearResponse = await fetch(
    `${restartedUrl}/tui/agent-models/${encodeURIComponent(agentB.agentID)}`,
    {
      method: "PUT",
      headers: { ...headers, "content-type": "application/json" },
      body: JSON.stringify({ preference: null }),
    },
  )
  if (!clearResponse.ok) {
    throw new Error(`Clearing /tui/agent-models/${agentB.agentID} failed with status ${clearResponse.status}`)
  }
  const cleared = (await clearResponse.json()) as AgentModelRow
  expect(cleared).toMatchObject({
    agentID: agentB.agentID,
    preference: null,
    preferenceAvailable: false,
    effective: { providerID: "fixture", modelID: "default", source: "default" },
  })
  const clearedSession = (
    await restartedClient.session.create(
      { title: "Agent B default model after clear", agent: agentB.agentID },
      { throwOnError: true },
    )
  ).data!
  await Promise.race([
    restartedClient.session.prompt(
      { sessionID: clearedSession.id, parts: [{ type: "text", text: "prompt Agent B after clear" }] },
      { throwOnError: true },
    ),
    Bun.sleep(5_000).then(() => {
      throw new Error("timed out waiting for cleared Agent B provider completion")
    }),
  ])
  expect(providerModels).toEqual(["selected", "default", "selected", "default"])

  const reselectionResponse = await fetch(
    `${restartedUrl}/tui/agent-models/${encodeURIComponent(agentB.agentID)}`,
    {
      method: "PUT",
      headers: { ...headers, "content-type": "application/json" },
      body: JSON.stringify({ preference: { providerID: "fixture", modelID: "selected" } }),
    },
  )
  if (!reselectionResponse.ok) {
    throw new Error(`Re-selecting /tui/agent-models/${agentB.agentID} failed with status ${reselectionResponse.status}`)
  }
  expect((await reselectionResponse.json()) as AgentModelRow).toMatchObject({
    agentID: agentB.agentID,
    preference: { providerID: "fixture", modelID: "selected" },
    preferenceAvailable: true,
    effective: { providerID: "fixture", modelID: "selected", source: "remembered" },
  })

  await setProviderModels(["default"])
  const staleUrl = await restart()
  const staleResponse = await fetch(`${staleUrl}/tui/agent-models`, { headers })
  if (!staleResponse.ok) {
    throw new Error(`GET /tui/agent-models after catalog change failed with status ${staleResponse.status}`)
  }
  const staleRows = (await staleResponse.json()) as AgentModelRow[]
  expect(staleRows.find((row) => row.agentID === agentB.agentID)).toMatchObject({
    agentID: agentB.agentID,
    preference: { providerID: "fixture", modelID: "selected" },
    preferenceAvailable: false,
    effective: { providerID: "fixture", modelID: "default", source: "default" },
  })
  const staleClient = createOpencodeClient({ baseUrl: staleUrl, directory: project })
  const staleSession = (
    await staleClient.session.create(
      { title: "Agent B default model after stale preference", agent: agentB.agentID },
      { throwOnError: true },
    )
  ).data!
  await Promise.race([
    staleClient.session.prompt(
      { sessionID: staleSession.id, parts: [{ type: "text", text: "prompt Agent B after stale preference" }] },
      { throwOnError: true },
    ),
    Bun.sleep(5_000).then(() => {
      throw new Error("timed out waiting for stale Agent B provider completion")
    }),
  ])
  expect(providerModels).toEqual(["selected", "default", "selected", "default", "default"])
}, 45_000)
