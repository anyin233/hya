import path from "node:path"
import { z } from "zod"

import { vcsInfo, type OpenCodeVcsInfo } from "./client_vcs"
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
    readonly list: () => Promise<OpenCodeClientResponse<readonly OpenCodeProject[]>>
  }
  readonly formatter: {
    readonly status: () => Promise<OpenCodeClientResponse<readonly unknown[]>>
  }
  readonly lsp: {
    readonly status: () => Promise<OpenCodeClientResponse<readonly unknown[]>>
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
      list: async () => ok([context.project]),
    },
    formatter: {
      status: async () => ok([]),
    },
    lsp: {
      status: async () => ok([]),
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
