import { expect, test } from "bun:test"

import { openCodeEventFromEnvelope } from "../src/event_converters"

test("converts assistant message finish events to OpenCode message updates", () => {
  // Given
  const envelope = {
    seq: 31,
    ts_millis: 44,
    event: {
      type: "message_finished",
      session: "session-1",
      message: "message-2",
      role: "assistant",
      finish: "tool_calls",
    },
  } as const

  // When
  const event = openCodeEventFromEnvelope(envelope)

  // Then
  expect(event).toEqual({
    id: "31",
    type: "message.updated",
    properties: {
      info: {
        id: "message-2",
        sessionID: "session-1",
        role: "assistant",
        time: { created: 44 },
        parentID: "",
        modelID: "unknown",
        providerID: "hya",
        mode: "build",
        path: { cwd: "", root: "" },
        cost: 0,
        tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
        finish: "tool-calls",
      },
    },
  })
})
