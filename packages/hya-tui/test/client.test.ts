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
  const session = await client.createSession("build", "offline/echo", "/work")
  const turn = await client.createTurn(session.id, "hello")

  expect(session.id).toBe("hysec_1")
  expect(turn.id).toBe("msg_1")
  expect(calls).toEqual([
    {
      url: "http://127.0.0.1:8080/v1/sessions",
      method: "POST",
      directory: "/work",
      body: { agent: "build", model: "offline/echo", workdir: "/work" },
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

test("lists saved provider names and stores/removes a key without returning its value", async () => {
  const calls: Array<{ method: string; path: string; body: unknown }> = []
  const fetcher: FetchLike = async (path, init) => {
    calls.push({ method: init?.method ?? "GET", path, body: init?.body ? JSON.parse(String(init.body)) : undefined })
    if (path.endsWith("/v1/auth")) return Response.json({ providerIds: ["anthropic"] })
    return Response.json({ status: "AUTH_STATUS_CREDENTIALED" })
  }
  const client = new HyaClient("http://127.0.0.1:8080", "/work", fetcher)
  expect(await client.listSavedKeys()).toEqual(["anthropic"])
  await client.setProviderKey("anthropic", "sk-secret")
  await client.removeProviderKey("anthropic")
  expect(calls).toEqual([
    { method: "GET", path: "http://127.0.0.1:8080/v1/auth", body: undefined },
    { method: "PUT", path: "http://127.0.0.1:8080/v1/auth/anthropic", body: { apiKey: "sk-secret" } },
    { method: "DELETE", path: "http://127.0.0.1:8080/v1/auth/anthropic", body: undefined },
  ])
})

test("treats a missing auth list as unavailable and reports empty HTTP errors", async () => {
  const missing = new HyaClient("http://127.0.0.1:8080", "/work", async () =>
    new Response(null, { status: 404, statusText: "Not Found" }))
  expect(await missing.listSavedKeys()).toBeNull()

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
