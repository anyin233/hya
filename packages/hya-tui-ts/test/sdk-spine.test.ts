import { expect, test } from "bun:test"
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

test("public hya entry drives the upstream SDK bootstrap and event reducer", async () => {
  const previousStateHome = process.env.XDG_STATE_HOME
  const temp = await realpath(await mkdtemp(path.join(os.tmpdir(), "hya-tui-sdk-")))
  const project = path.join(temp, "project")
  await mkdir(project)
  await mkdir(path.join(temp, "state", "hya"), { recursive: true })
  await writeFile(path.join(temp, "state", "hya", "kv.json"), "{}")
  process.env.XDG_STATE_HOME = path.join(temp, "state")
  const { launch } = await import("../src/main")
  const { observeSdkSpine } = await import("../src/hya/sdk-spine")
  const requests: Array<{ path: string; directory: string | null }> = []
  const workflowIdentity = {
    source: "project:release",
    name: "release",
    revision: "ab".repeat(32),
  }
  const initialSession = {
    id: "ses_live",
    title: "Live session",
    slug: "live-session",
    projectID: "project",
    directory: project,
    version: "test",
    time: { created: 1, updated: 2 },
    workflow: { selection: workflowIdentity, availability: "available" },
  }
  const session = {
    ...initialSession,
    time: { created: 1, updated: 3 },
    workflow: {
      selection: workflowIdentity,
      availability: "available",
      run: {
        id: "run-1",
        workflow: workflowIdentity,
        request_hash: "hash",
        owner: "owner",
        status: "completed",
        stages: [],
      },
    },
  }
  const workflowStates = new Set<string>()
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      const url = new URL(request.url)
      requests.push({
        path: url.pathname,
        directory: url.searchParams.get("directory") ?? request.headers.get("x-opencode-directory"),
      })
      if (url.pathname === "/global/event") {
        return new Response(
          new ReadableStream({
            start(controller) {
              setTimeout(() => {
                controller.enqueue(
                  new TextEncoder().encode(
                    `data: ${JSON.stringify({
                      directory: project,
                      payload: { id: "evt_live", type: "session.updated", properties: { info: session } },
                    })}\n\n`,
                  ),
                )
              }, 50)
            },
          }),
          { headers: { "content-type": "text/event-stream" } },
        )
      }

      if (
        url.pathname === "/experimental/console" ||
        url.pathname === "/provider/auth" ||
        url.pathname === "/global/upgrade" ||
        url.pathname.startsWith("/experimental/workspace") ||
        /\/session\/[^/]+\/share$/.test(url.pathname)
      ) {
        return new Response("unsupported", { status: 500 })
      }

      const location = { directory: project, project: { id: "project", directory: project } }
      const body = (() => {
        if (url.pathname === "/path")
          return { home: os.homedir(), state: "", config: "", worktree: project, directory: project }
        if (url.pathname === "/project/current") return { id: "project", worktree: project }
        if (url.pathname.includes("/directories")) return [{ directory: project }]
        if (url.pathname === "/config/providers") return { providers: [], default: {} }
        if (url.pathname === "/provider") return { all: [], default: {}, connected: [] }
        if (url.pathname === "/experimental/capabilities") return { backgroundSubagents: false }
        if (url.pathname === "/session") return [initialSession]
        if (url.pathname === "/config" || url.pathname === "/session/status") return {}
        if (url.pathname === "/mcp" || url.pathname === "/experimental/resource") return {}
        if (url.pathname === "/vcs") return { branch: "main" }
        if (url.pathname === "/api/location") return location
        if (url.pathname.startsWith("/api/")) return { location, data: [] }
        return []
      })()
      return Response.json(body)
    },
  })
  const cwd = process.cwd()

  try {
    await launch(["--url", server.url.toString(), "--project", project, "--continue"], async (input) => {
      expect(input.url).toBe(server.url.toString())
      expect(input.directory).toBe(project)
      expect(input.config.mouse).toBe(true)
      expect(typeof input.pluginHost.start).toBe("function")
      await observeSdkSpine(input, (state) => {
        const current = state.sync.session.find((item) => item.id === session.id) as
          | ({ workflow?: { selection?: unknown; run?: { status?: string } } } & object)
          | undefined
        const workflow = current?.workflow
        if (workflow?.run?.status) workflowStates.add(workflow.run.status)
        else if (workflow?.selection) workflowStates.add("ready")
        return workflowStates.has("ready") && workflowStates.has("completed")
      })
    })
    expect(workflowStates).toEqual(new Set(["ready", "completed"]))

    const paths = new Set(requests.map((request) => request.path))
    for (const required of [
      "/path",
      "/config",
      "/config/providers",
      "/provider",
      "/agent",
      "/session",
      "/api/location",
      "/api/model",
      "/global/event",
    ]) {
      expect(paths).toContain(required)
    }
    expect(new Set(requests.map((request) => request.directory))).toEqual(new Set([project]))
    expect(
      requests.some(
        (request) =>
          request.path === "/experimental/console" ||
          request.path === "/provider/auth" ||
          request.path === "/global/upgrade" ||
          request.path.startsWith("/experimental/workspace") ||
          /\/session\/[^/]+\/share$/.test(request.path),
      ),
    ).toBe(false)
  } finally {
    process.chdir(cwd)
    if (previousStateHome === undefined) delete process.env.XDG_STATE_HOME
    else process.env.XDG_STATE_HOME = previousStateHome
    server.stop(true)
    await rm(temp, { recursive: true, force: true })
  }
})
