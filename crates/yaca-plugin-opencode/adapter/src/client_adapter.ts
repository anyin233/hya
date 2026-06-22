import { z } from "zod"

import type { TextSink } from "./runtime_types"

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

export type OpenCodeClientAdapter = {
  readonly app: {
    readonly log: (options: unknown) => Promise<OpenCodeClientResponse<boolean>>
  }
}

export function createOpenCodeClientAdapter(
  stderr: TextSink,
): OpenCodeClientAdapter {
  return {
    app: {
      log: async (options) => appLog(stderr, options),
    },
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
