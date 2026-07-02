import path from "node:path"
import { z } from "zod"

import { vcsInfo, type CompatVcsInfo } from "./client_vcs"
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

type CompatBadRequest = {
  readonly name: "BadRequest"
  readonly data: {
    readonly message: string
    readonly kind: "Body"
  }
}

type CompatClientResponse<T> =
  | {
      readonly data: T
      readonly error?: undefined
      readonly response: Response
    }
  | {
      readonly data?: undefined
      readonly error: CompatBadRequest
      readonly response: Response
    }

export type CompatProject = {
  readonly id: string
  readonly worktree: string
  readonly vcsDir?: string
  readonly vcs?: "git"
  readonly time: {
    readonly created: number
    readonly initialized?: number
  }
}

type CompatPath = {
  readonly home: string
  readonly state: string
  readonly config: string
  readonly worktree: string
  readonly directory: string
}

type CompatClientContext = {
  readonly env: RuntimeEnv
  readonly directory: string
  readonly worktree: string
  readonly project: CompatProject
}

export type CompatClientAdapter = {
  readonly app: {
    readonly log: (options: unknown) => Promise<CompatClientResponse<boolean>>
    readonly agents: () => Promise<CompatClientResponse<readonly unknown[]>>
    readonly skills: () => Promise<CompatClientResponse<readonly unknown[]>>
  }
  readonly config: {
    readonly get: () => Promise<CompatClientResponse<Readonly<Record<string, unknown>>>>
  }
  readonly auth: {
    readonly set: (options: unknown) => Promise<CompatClientResponse<boolean>>
    readonly remove: (options: unknown) => Promise<CompatClientResponse<boolean>>
  }
  readonly path: {
    readonly get: () => Promise<CompatClientResponse<CompatPath>>
  }
  readonly project: {
    readonly current: () => Promise<CompatClientResponse<CompatProject>>
    readonly list: () => Promise<CompatClientResponse<readonly CompatProject[]>>
  }
  readonly formatter: {
    readonly status: () => Promise<CompatClientResponse<readonly unknown[]>>
  }
  readonly lsp: {
    readonly status: () => Promise<CompatClientResponse<readonly unknown[]>>
  }
  readonly tool: {
    readonly ids: () => Promise<CompatClientResponse<readonly string[]>>
  }
  readonly vcs: {
    readonly get: () => Promise<CompatClientResponse<CompatVcsInfo>>
  }
}

export function createCompatProject(
  env: RuntimeEnv,
  worktree: string,
): CompatProject {
  return {
    id: env.HYA_PROJECT_ID ?? worktree,
    worktree,
    time: {
      created: Date.now(),
    },
  }
}

export function createCompatClientAdapter(
  stderr: TextSink,
  context: CompatClientContext,
): CompatClientAdapter {
  return {
    app: {
      log: async (options) => appLog(stderr, options),
      agents: async () => ok([]),
      skills: async () => ok([]),
    },
    config: {
      get: async () => ok({}),
    },
    auth: {
      set: async () => badRequest("auth mutation is unavailable in the adapter"),
      remove: async () => badRequest("auth mutation is unavailable in the adapter"),
    },
    path: {
      get: async () => ok(pathInfo(context)),
    },
    project: {
      current: async () => ok(context.project),
      list: async () => ok([context.project]),
    },
    formatter: {
      status: async () => ok([]),
    },
    lsp: {
      status: async () => ok([]),
    },
    tool: {
      ids: async () => ok([]),
    },
    vcs: {
      get: async () => ok(await vcsInfo(context)),
    },
  }
}

function ok<T>(data: T): CompatClientResponse<T> {
  return {
    data,
    response: new Response(null, { status: 200 }),
  }
}

function badRequest<T>(message: string): CompatClientResponse<T> {
  return {
    error: {
      name: "BadRequest",
      data: { message, kind: "Body" },
    },
    response: new Response(null, { status: 400 }),
  }
}

async function appLog(
  stderr: TextSink,
  options: unknown,
): Promise<CompatClientResponse<boolean>> {
  const parsed = AppLogOptionsSchema.safeParse(options)
  if (!parsed.success) {
    return badRequest(parsed.error.message)
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
  return `compat plugin ${body.level} ${body.service}: ${body.message}${extra}\n`
}

function pathInfo(context: CompatClientContext): CompatPath {
  const home = context.env.HOME ?? context.directory
  return {
    home,
    state: path.join(context.env.XDG_STATE_HOME ?? path.join(home, ".local", "state"), "compat"),
    config: path.join(context.env.XDG_CONFIG_HOME ?? path.join(home, ".config"), "compat"),
    worktree: context.worktree,
    directory: context.directory,
  }
}
