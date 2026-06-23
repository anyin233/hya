import path from "node:path"
import { z } from "zod"

import type { RuntimeEnv, TextSink } from "./runtime_types"

const LogLevelSchema = z.enum(["debug", "info", "error", "warn"])

const AppLogBodySchema = z
  .object({
    service: z.string(),
    level: LogLevelSchema,
    message: z.string(),
    extra: z.record(z.string(), z.unknown()).optional(),
  })
  .strict()

const AppLogOptionsSchema = z.union([
  AppLogBodySchema,
  z
    .object({
      body: AppLogBodySchema,
    })
    .strict(),
])

type AppLogBody = z.infer<typeof AppLogBodySchema>

type OpenCodeBadRequest = {
  readonly name: "BadRequest"
  readonly data: {
    readonly message: string
    readonly kind: "Body"
  }
}

type OpenCodeClientResponse<T> =
  | {
      readonly data: T
      readonly error?: undefined
      readonly response: Response
    }
  | {
      readonly data?: undefined
      readonly error: OpenCodeBadRequest
      readonly response: Response
    }

export type OpenCodeProject = {
  readonly id: string
  readonly worktree: string
  readonly vcsDir?: string
  readonly vcs?: "git"
  readonly time: {
    readonly created: number
    readonly initialized?: number
  }
}

type OpenCodePath = {
  readonly home: string
  readonly state: string
  readonly config: string
  readonly worktree: string
  readonly directory: string
}

type OpenCodeVcsInfo = {
  readonly branch: string
  readonly default_branch?: string
}

type OpenCodeClientContext = {
  readonly env: RuntimeEnv
  readonly directory: string
  readonly worktree: string
  readonly project: OpenCodeProject
}

export type OpenCodeClientAdapter = {
  readonly app: {
    readonly log: (options: unknown) => Promise<OpenCodeClientResponse<boolean>>
  }
  readonly path: {
    readonly get: () => Promise<OpenCodeClientResponse<OpenCodePath>>
  }
  readonly project: {
    readonly current: () => Promise<OpenCodeClientResponse<OpenCodeProject>>
  }
  readonly vcs: {
    readonly get: () => Promise<OpenCodeClientResponse<OpenCodeVcsInfo>>
  }
}

export function createOpenCodeProject(
  env: RuntimeEnv,
  worktree: string,
): OpenCodeProject {
  return {
    id: env.YACA_PROJECT_ID ?? worktree,
    worktree,
    time: {
      created: Date.now(),
    },
  }
}

export function createOpenCodeClientAdapter(
  stderr: TextSink,
  context: OpenCodeClientContext,
): OpenCodeClientAdapter {
  return {
    app: {
      log: async (options) => appLog(stderr, options),
    },
    path: {
      get: async () => ok(pathInfo(context)),
    },
    project: {
      current: async () => ok(context.project),
    },
    vcs: {
      get: async () => ok(await vcsInfo(context)),
    },
  }
}

function ok<T>(data: T): OpenCodeClientResponse<T> {
  return {
    data,
    response: new Response(null, { status: 200 }),
  }
}

async function appLog(
  stderr: TextSink,
  options: unknown,
): Promise<OpenCodeClientResponse<boolean>> {
  const parsed = AppLogOptionsSchema.safeParse(options)
  if (!parsed.success) {
    return {
      error: {
        name: "BadRequest",
        data: { message: parsed.error.message, kind: "Body" },
      },
      response: new Response(null, { status: 400 }),
    }
  }

  const body = "body" in parsed.data ? parsed.data.body : parsed.data
  await Promise.resolve(stderr.write(formatAppLog(body)))
  return {
    data: true,
    response: new Response(null, { status: 200 }),
  }
}

function formatAppLog(body: AppLogBody): string {
  const extra = body.extra === undefined ? "" : ` ${JSON.stringify(body.extra)}`
  return `opencode plugin ${body.level} ${body.service}: ${body.message}${extra}\n`
}

function pathInfo(context: OpenCodeClientContext): OpenCodePath {
  const home = context.env.HOME ?? context.directory
  return {
    home,
    state: path.join(context.env.XDG_STATE_HOME ?? path.join(home, ".local", "state"), "opencode"),
    config: path.join(context.env.XDG_CONFIG_HOME ?? path.join(home, ".config"), "opencode"),
    worktree: context.worktree,
    directory: context.directory,
  }
}

async function vcsInfo(
  context: OpenCodeClientContext,
): Promise<OpenCodeVcsInfo> {
  const [branch, defaultBranch] = await Promise.all([
    gitText(context, ["branch", "--show-current"]),
    gitDefaultBranch(context),
  ])
  if (defaultBranch === "") {
    return { branch }
  }
  return { branch, default_branch: defaultBranch }
}

async function gitDefaultBranch(context: OpenCodeClientContext): Promise<string> {
  const remote = await primaryRemote(context)
  if (remote !== "") {
    const branch = await gitText(context, ["symbolic-ref", "--short", `refs/remotes/${remote}/HEAD`])
    const prefix = `${remote}/`
    if (branch.startsWith(prefix)) {
      return branch.slice(prefix.length)
    }
    if (branch !== "") {
      return branch
    }
  }

  const refs = lines(await gitText(context, ["for-each-ref", "--format=%(refname:short)", "refs/heads"]))
  const configured = await gitText(context, ["config", "init.defaultBranch"])
  if (configured !== "" && refs.includes(configured)) {
    return configured
  }
  if (refs.includes("main")) {
    return "main"
  }
  if (refs.includes("master")) {
    return "master"
  }
  return ""
}

async function primaryRemote(context: OpenCodeClientContext): Promise<string> {
  const remotes = lines(await gitText(context, ["remote"]))
  if (remotes.includes("origin")) {
    return "origin"
  }
  if (remotes.length === 1) {
    return remotes[0] ?? ""
  }
  if (remotes.includes("upstream")) {
    return "upstream"
  }
  return remotes[0] ?? ""
}

function lines(text: string): readonly string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
}

async function gitText(
  context: OpenCodeClientContext,
  args: readonly string[],
): Promise<string> {
  const proc = Bun.spawn(["git", "-C", context.directory, ...args], {
    stdout: "pipe",
    stderr: "pipe",
  })
  const [stdout, , exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    new Response(proc.stderr).text(),
    proc.exited,
  ])
  if (exitCode !== 0) {
    return ""
  }
  return stdout.trim()
}
