import { expect, test } from "bun:test"
import { HyaClient, SseDecoder, parseApiCommand, type FetchLike } from "../src/client"

test("creates a session and admits a prompt through scoped v1 requests", async () => {
  const calls: Array<{ url: string; method: string; directory: string | null; body: unknown }> = []
  const fetcher: FetchLike = async (input, init) => {
    const url = String(input)
    const headers = new Headers(init?.headers)
    calls.push({
      url,
      method: init?.method ?? "GET",
      directory: headers.get("x-hya-directory"),
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    })
    return Response.json(url.endsWith("/turns")
      ? { turn: { id: "msg_1", state: "TURN_STATE_RUNNING" } }
      : { session: { id: "hysec_1", agent: "build", workdir: "/work" } })
  }

  const client = new HyaClient("http://127.0.0.1:8080/", "/work", fetcher)
  const session = await client.createSession("build", "offline/echo", { workdir: "/work" })
  const turn = await client.createTurn(session.id, "hello")

  expect(session.id).toBe("hysec_1")
  expect(turn.id).toBe("msg_1")
  expect(calls).toEqual([
    {
      url: "http://127.0.0.1:8080/v1/sessions",
      method: "POST",
      directory: "/work",
      // A local start: the server reuses the Project containing the cwd or creates one (EnsureProjectForPath).
      body: { agent: "build", model: "offline/echo", workdir: "/work", kind: "SESSION_KIND_PROJECT" },
    },
    {
      url: "http://127.0.0.1:8080/v1/sessions/hysec_1/turns",
      method: "POST",
      directory: "/work",
      body: { prompt: { text: "hello" } },
    },
  ])
})

test("getVcsStatus scopes GetVcsStatus to the client's directory", async () => {
  const calls: string[] = []
  const fetcher: FetchLike = async (input) => {
    calls.push(String(input))
    return Response.json({ branch: "main", dirty: 2 })
  }
  const client = new HyaClient("http://127.0.0.1:8080/", "/work/dir", fetcher)
  const status = await client.getVcsStatus()
  expect(status).toEqual({ branch: "main", dirty: 2 })
  expect(calls).toEqual(["http://127.0.0.1:8080/v1/vcs?directory=%2Fwork%2Fdir"])
})

test("decodes SSE frames split across transport chunks", () => {
  const decoder = new SseDecoder()
  expect(decoder.push(": keepalive\n\ndata: {\"event\":{\"seq\":\"12\",\"session\":\"s\"}}\n\n")).toEqual([
    { event: { seq: "12", session: "s" } },
  ])
  expect(decoder.push("data: {\"resync\":{\"lastSeq\":\"12\"}}\n")).toEqual([])
  expect(decoder.push("\n")).toEqual([{ resync: { lastSeq: "12" } }])
})

test("parses only scoped JSON API commands", () => {
  expect(parseApiCommand('/api POST /v1/sessions {"agent":"build"}')).toEqual({
    method: "POST", path: "/v1/sessions", body: { agent: "build" },
  })
  expect(() => parseApiCommand("/api GET https://example.com/")).toThrow("/v1/")
  expect(() => parseApiCommand("/api GET /v1/sessions {} ")).toThrow("GET")
})

test("loads later transcript pages so new messages stay visible", async () => {
  const paths: string[] = []
  const fetcher: FetchLike = async (path) => {
    paths.push(path)
    return Response.json(paths.length === 1
      ? { messages: [{ id: "old", role: "ROLE_USER" }], page: { nextCursor: "next", hasMore: true } }
      : { messages: [{ id: "new", role: "ROLE_ASSISTANT" }], page: { hasMore: false } })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "/work", fetcher)
  expect((await client.listMessages("hysec_1")).map((message) => message.id)).toEqual(["old", "new"])
  expect(paths[1]).toContain("page.cursor=next")
})

test("forwards backend slash commands as command turns", async () => {
  let body: unknown
  const fetcher: FetchLike = async (_path, init) => {
    body = JSON.parse(String(init?.body))
    return Response.json({ turn: { id: "msg_command", state: "TURN_STATE_RUNNING" } })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "/work", fetcher)
  const turn = await client.createCommandTurn("hysec_1", "compact", "now")
  expect(turn.id).toBe("msg_command")
  expect(body).toEqual({ command: { command: "compact", arguments: "now" } })
})

test("provider routes: upsert, refresh, key set/remove, model override set/remove, and a test that can be aborted", async () => {
  const calls: Array<{ method: string; path: string; body: unknown }> = []
  let signal: AbortSignal | undefined
  const fetcher: FetchLike = async (path, init) => {
    calls.push({ method: init?.method ?? "GET", path, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    if (path.endsWith("/test")) {
      signal = init?.signal ?? undefined
      return Response.json({ ok: true, text: "Hi", finishReason: "length", latencyMs: 12 })
    }
    if (path.includes("/v1/auth/")) return Response.json({ status: "AUTH_STATUS_CREDENTIALED", discovery: { ok: true, result: "models", modelCount: 1 } })
    return Response.json({ provider: { summary: { id: "gw" } }, discovery: { ok: false, result: "unavailable", errorMessage: "refused" } })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "/work", fetcher)
  const added = await client.upsertProvider("gw", { kind: "openai", baseUrl: "http://h/v1", apiKey: "sk-secret" })
  expect(added.discovery?.errorMessage).toBe("refused")
  await client.upsertProvider("gw", { kind: "openai", baseUrl: "http://h/v1" })
  await client.refreshProvider("gw")
  expect((await client.setProviderKey("gw", "sk-2")).discovery?.modelCount).toBe(1)
  await client.removeProviderKey("gw")
  await client.setProviderModel("gw", { modelId: "vendor/m:1", displayName: "M", contextLimit: 8000, reasoning: true })
  await client.removeProviderModel("gw", "vendor/m:1")
  const abort = new AbortController()
  expect(await client.testProviderModel("gw", "vendor/m:1", abort.signal)).toEqual({ ok: true, text: "Hi", finishReason: "length", latencyMs: 12 })
  expect(signal).toBe(abort.signal)
  const base = "http://127.0.0.1:8080/v1"
  expect(calls).toEqual([
    { method: "PUT", path: `${base}/providers/gw`, body: { kind: "openai", baseUrl: "http://h/v1", apiKey: "sk-secret" } },
    { method: "PUT", path: `${base}/providers/gw`, body: { kind: "openai", baseUrl: "http://h/v1" } },
    { method: "POST", path: `${base}/providers/gw/refresh`, body: {} },
    { method: "PUT", path: `${base}/auth/gw`, body: { apiKey: "sk-2" } },
    { method: "DELETE", path: `${base}/auth/gw`, body: undefined },
    { method: "PUT", path: `${base}/providers/gw/models`, body: { modelId: "vendor/m:1", displayName: "M", contextLimit: 8000, reasoning: true } },
    // Model ids may hold `/` and `:`: they travel in the query, percent-encoded.
    { method: "DELETE", path: `${base}/providers/gw/models?modelId=vendor%2Fm%3A1`, body: undefined },
    { method: "POST", path: `${base}/providers/gw/test`, body: { modelId: "vendor/m:1" } },
  ])
})

test("reports empty HTTP errors", async () => {
  const failed = new HyaClient("http://127.0.0.1:8080", "/work", async () =>
    new Response(null, { status: 503, statusText: "Service Unavailable" }))
  await expect(failed.bootstrap()).rejects.toThrow("GET /v1/bootstrap: HTTP 503 Service Unavailable")

  const invalid = new HyaClient("http://127.0.0.1:8080", "/work", async () =>
    new Response("gateway error", { status: 502, statusText: "Bad Gateway" }))
  await expect(invalid.bootstrap()).rejects.toThrow("GET /v1/bootstrap: HTTP 502 Bad Gateway")
})

test("admits a shell turn and finds files through scoped v1 requests", async () => {
  const calls: Array<{ url: string; method: string; directory: string | null; body: unknown }> = []
  const fetcher: FetchLike = async (input, init) => {
    const url = String(input)
    calls.push({
      url,
      method: init?.method ?? "GET",
      directory: new Headers(init?.headers).get("x-hya-directory"),
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    })
    return Response.json(url.includes("/fs/find")
      ? { paths: ["src/main.ts"] }
      : { turn: { id: "msg_a", state: "TURN_STATE_FINISHED", finish: "FINISH_REASON_STOP" } })
  }
  const client = new HyaClient("http://h", "/work", fetcher)
  const turn = await client.createShellTurn("hysec_1", "echo hi", "build", { providerId: "fake", modelId: "model" })
  expect(turn.id).toBe("msg_a")
  expect(await client.findFiles("**/*ma in*", 20)).toEqual(["src/main.ts"])
  expect(calls).toEqual([
    {
      url: "http://h/v1/sessions/hysec_1/turns",
      method: "POST",
      directory: "/work",
      body: { shell: { command: "echo hi", agent: "build", model: { providerId: "fake", modelId: "model" } } },
    },
    { url: "http://h/v1/fs/find?pattern=**%2F*ma%20in*&limit=20", method: "GET", directory: "/work", body: undefined },
  ])
})

test("streamSession opts in to descendant ask frames with includeDescendants=true", async () => {
  const urls: string[] = []
  const client = new HyaClient("http://127.0.0.1:1", "/w", async (input) => {
    urls.push(input)
    return new Response("", { headers: { "content-type": "text/event-stream" } })
  })
  await client.streamSession("hysec_1", "4", () => undefined, new AbortController().signal, undefined, true)
  await client.streamSession("hysec_1", "4", () => undefined, new AbortController().signal)
  expect(urls).toEqual([
    "http://127.0.0.1:1/v1/sessions/hysec_1/events/stream?sinceSeq=4&includeDescendants=true",
    "http://127.0.0.1:1/v1/sessions/hysec_1/events/stream?sinceSeq=4",
  ])
})

test("streamGlobal subscribes with interactionsOnly=true (live ask frames and catalogUpdated only)", async () => {
  const urls: string[] = []
  const frames: unknown[] = []
  const client = new HyaClient("http://127.0.0.1:1", "/w", async (input) => {
    urls.push(input)
    return new Response(`data: {"event":{"session":"hysec_9","permissionRequested":{"interaction":{"id":"perm_1","title":"bash"}}}}\n\n`, { headers: { "content-type": "text/event-stream" } })
  })
  let opened = false
  await client.streamGlobal((frame) => { frames.push(frame) }, new AbortController().signal, () => { opened = true })
  expect(urls).toEqual(["http://127.0.0.1:1/v1/events/stream?interactionsOnly=true"])
  expect(opened).toBe(true)
  expect(frames).toEqual([{ event: { session: "hysec_9", permissionRequested: { interaction: { id: "perm_1", title: "bash" } } } }])
})

test("revertSession and forkSession post RevertSession / ForkSession bodies", async () => {
  const calls: Array<{ url: string; method: string; body: unknown }> = []
  const fetcher: FetchLike = async (input, init) => {
    const url = String(input)
    calls.push({ url, method: init?.method ?? "GET", body: init?.body ? JSON.parse(String(init.body)) : undefined })
    return Response.json(url.endsWith("/fork")
      ? { session: { id: "hysec_2", agent: "build", workdir: "/work", forkedFrom: { session: "hysec_1", messageId: "msg_2" } }, promptText: "again" }
      : { session: { id: "hysec_1", agent: "build", workdir: "/work", revert: { messageId: "msg_2", text: "again", hiddenMessages: 2 } }, files: [{ path: "/work/a", action: "restored" }] })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "/work", fetcher)
  const reverted = await client.revertSession("hysec_1", {})
  expect(reverted.session.revert?.text).toBe("again")
  expect(reverted.files?.[0]?.action).toBe("restored")
  await client.revertSession("hysec_1", { messageId: "msg_2" })
  await client.revertSession("hysec_1", { undo: true })
  const fork = await client.forkSession("hysec_1", "msg_2")
  expect(fork.promptText).toBe("again")
  expect(fork.session.forkedFrom?.session).toBe("hysec_1")
  await client.forkSession("hysec_1")
  expect(calls).toEqual([
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1/revert", method: "POST", body: {} },
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1/revert", method: "POST", body: { messageId: "msg_2" } },
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1/revert", method: "POST", body: { undo: true } },
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1/fork", method: "POST", body: { messageId: "msg_2" } },
    { url: "http://127.0.0.1:8080/v1/sessions/hysec_1/fork", method: "POST", body: {} },
  ])
})

function recordingFetcher(reply: (url: string, method: string) => unknown) {
  const calls: Array<{ url: string; method: string; directory: string | null; body: unknown }> = []
  const fetcher: FetchLike = async (input, init) => {
    const url = String(input)
    const method = init?.method ?? "GET"
    calls.push({ url, method, directory: new Headers(init?.headers).get("x-hya-directory"), body: init?.body ? JSON.parse(String(init.body)) : undefined })
    return Response.json(reply(url, method) ?? {})
  }
  return { calls, fetcher }
}

const project = { id: "prj_1", name: "work", roots: ["/work", "/docs"], sessionCount: 2, busy: true }

test("createSession places a session in a Project, at a workdir, or as a temporary session", async () => {
  const { calls, fetcher } = recordingFetcher(() => ({ session: { id: "s", agent: "build", workdir: "/work" } }))
  const client = new HyaClient("http://h", "/work", fetcher)
  await client.createSession("build", "m/x", { projectId: "prj_1", workdir: "/work/sub" })
  await client.createSession("build", "m/x", { projectId: "prj_1" })
  await client.createSession("build", "m/x", { temporary: true })
  expect(calls.map((call) => call.body)).toEqual([
    { agent: "build", model: "m/x", kind: "SESSION_KIND_PROJECT", projectId: "prj_1", workdir: "/work/sub" },
    { agent: "build", model: "m/x", kind: "SESSION_KIND_PROJECT", projectId: "prj_1" },
    { agent: "build", model: "m/x", kind: "SESSION_KIND_TEMPORARY" },
  ])
})

test("setDirectory changes the x-hya-directory scope of later requests and streams", async () => {
  const { calls, fetcher } = recordingFetcher(() => ({ branch: "main" }))
  const client = new HyaClient("http://h", "/work", fetcher)
  expect(client.directory).toBe("/work")
  client.setDirectory("/docs")
  expect(client.directory).toBe("/docs")
  await client.getVcsStatus()
  expect(calls).toEqual([{ url: "http://h/v1/vcs?directory=%2Fdocs", method: "GET", directory: "/docs", body: undefined }])
})

test("Project rpcs use the v1 routes and unwrap their responses", async () => {
  const { calls, fetcher } = recordingFetcher((url, method) => {
    if (url.includes("/v1/projects?")) return { projects: [project], page: { hasMore: false } }
    if (url.includes("/v1/projects/resolve")) return url.includes("nowhere") ? {} : { project }
    if (url.endsWith("/v1/projects/ensure")) return { project, created: true }
    if (method === "DELETE") return {}
    return project
  })
  const client = new HyaClient("http://h", "/work", fetcher)
  expect(await client.listProjects()).toEqual([project])
  expect(await client.getProject("prj_1")).toEqual(project)
  expect(await client.createProject({ name: "work", roots: ["/work", "/docs"] })).toEqual(project)
  expect(await client.updateProject("prj_1", { name: "renamed" })).toEqual(project)
  await client.deleteProject("prj_1")
  expect(await client.resolveProject("/work/sub")).toEqual(project)
  expect(await client.resolveProject("/nowhere")).toBeUndefined()
  expect(await client.ensureProjectForPath("/work")).toEqual({ project, created: true })
  expect(calls.map((call) => [call.method, call.url.replace("http://h", ""), call.body])).toEqual([
    ["GET", "/v1/projects?page.limit=500", undefined],
    ["GET", "/v1/projects/prj_1", undefined],
    ["POST", "/v1/projects", { name: "work", roots: ["/work", "/docs"] }],
    ["PATCH", "/v1/projects/prj_1", { name: "renamed" }],
    ["DELETE", "/v1/projects/prj_1", undefined],
    ["GET", "/v1/projects/resolve?path=%2Fwork%2Fsub", undefined],
    ["GET", "/v1/projects/resolve?path=%2Fnowhere", undefined],
    ["POST", "/v1/projects/ensure", { path: "/work" }],
  ])
})

test("listSessions filters by Project", async () => {
  const { calls, fetcher } = recordingFetcher(() => ({ sessions: [{ id: "s", agent: "build", workdir: "/work", projectId: "prj_1" }] }))
  const client = new HyaClient("http://h", "/work", fetcher)
  expect((await client.listSessions({ projectId: "prj_1" })).map((row) => row.id)).toEqual(["s"])
  await client.listSessions()
  expect(calls.map((call) => call.url.replace("http://h", ""))).toEqual([
    "/v1/sessions?page.limit=500&projectId=prj_1",
    "/v1/sessions?page.limit=500",
  ])
})

test("readFile sends its own scope and cap and decodes the size; an empty scope sends no x-hya-directory", async () => {
  const calls: Array<{ url: string; directory: string | null }> = []
  const fetcher: FetchLike = async (input, init) => {
    const url = String(input)
    calls.push({ url, directory: new Headers(init?.headers).get("x-hya-directory") })
    if (url.includes("/v1/fs/read")) return Response.json({ content: Buffer.from([1, 2, 3]).toString("base64"), mime: "image/png" })
    return Response.json({ paths: [] })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "", fetcher)
  await client.findFiles("**/*a*", 5)
  const read = await client.readFile("shots/a b.png", { directory: "/srv/app", maxBytes: 11 })
  expect(read).toEqual({ data: "AQID", size: 3, text: false, mime: "image/png" })
  expect(calls).toEqual([
    { url: "http://127.0.0.1:8080/v1/fs/find?pattern=**%2F*a*&limit=5", directory: null },
    { url: "http://127.0.0.1:8080/v1/fs/read?path=shots%2Fa%20b.png&maxBytes=11", directory: "/srv/app" },
  ])
})
