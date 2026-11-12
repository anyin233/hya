import { runEventHooks } from "./event_hooks"
import type { JsonRpcNotification } from "./protocol"
import type { HandledRequest, RequestContext } from "./runtime_types"
import { isEventNotificationParams } from "./wire.gen"

export async function handleEventNotification(
  request: JsonRpcNotification,
  context: RequestContext,
): Promise<HandledRequest> {
  if (isEventNotificationParams(request.params)) {
    await runEventHooks(context.hooks, request.params.envelope)
  }
  return { response: "", shouldExit: false }
}
