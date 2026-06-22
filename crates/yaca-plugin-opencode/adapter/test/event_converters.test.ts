import { expect, test } from "bun:test"

import { openCodeEventFromEnvelope } from "../src/event_converters"

test("converts yaca lifecycle events to OpenCode events", () => {
  expect(
    openCodeEventFromEnvelope({
      seq: 21,
      ts_millis: 30,
      event: {
        type: "session_titled",
        session: "session-1",
        title: "Investigate parity",
      },
    }),
  ).toEqual({
    id: "21",
    type: "session.updated",
    properties: {
      info: {
        id: "session-1",
        projectID: "session-1",
        directory: "",
        title: "Investigate parity",
        version: "0",
        time: { created: 30, updated: 30 },
      },
    },
  })

  expect(
    openCodeEventFromEnvelope({
      seq: 22,
      ts_millis: 31,
      event: {
        type: "message_started",
        session: "session-1",
        message: "message-1",
        role: "user",
      },
    }),
  ).toEqual({
    id: "22",
    type: "message.updated",
    properties: {
      info: {
        id: "message-1",
        sessionID: "session-1",
        role: "user",
        time: { created: 31 },
        agent: "yaca",
        model: { providerID: "yaca", modelID: "unknown" },
      },
    },
  })

  expect(
    openCodeEventFromEnvelope({
      seq: 23,
      ts_millis: 32,
      event: {
        type: "reasoning_delta",
        session: "session-1",
        message: "message-2",
        part: "part-1",
        delta: "thinking",
      },
    }),
  ).toEqual({
    id: "23",
    type: "message.part.updated",
    properties: {
      part: {
        id: "part-1",
        sessionID: "session-1",
        messageID: "message-2",
        type: "reasoning",
        text: "thinking",
        time: { start: 32 },
      },
      delta: "thinking",
    },
  })

  expect(
    openCodeEventFromEnvelope({
      seq: 24,
      ts_millis: 33,
      event: {
        type: "tool_call_requested",
        session: "session-1",
        message: "message-2",
        part: "part-2",
        call: "call-1",
        name: "read",
        input: { path: "README.md" },
      },
    }),
  ).toEqual({
    id: "24",
    type: "message.part.updated",
    properties: {
      part: {
        id: "part-2",
        sessionID: "session-1",
        messageID: "message-2",
        type: "tool",
        callID: "call-1",
        tool: "read",
        state: {
          status: "running",
          input: { path: "README.md" },
          time: { start: 33 },
        },
      },
    },
  })
})
