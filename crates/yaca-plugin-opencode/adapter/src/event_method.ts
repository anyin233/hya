import { z } from "zod"

import { runEventHooks } from "./event_hooks"
import type { JsonRpcNotification } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"

const EventRecordSchema = z.object({ type: z.string() }).catchall(z.unknown())

const EventNotificationParamsSchema = z
  .object({
    envelope: z
      .object({
        seq: z.union([z.number().int().nonnegative(), z.string()]),
        ts_millis: z.number().int(),
        event: EventRecordSchema,
      })
      .strict(),
  })
  .strict()

export async function handleEventNotification(
  request: JsonRpcNotification,
  context: RequestContext,
): Promise<HandledRequest> {
  const params = EventNotificationParamsSchema.safeParse(request.params)
  if (params.success) {
    await runEventHooks(context.hooks, params.data.envelope)
  }
  return { response: "", shouldExit: false }
}
