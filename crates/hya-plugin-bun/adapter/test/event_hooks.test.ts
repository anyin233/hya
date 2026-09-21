import { afterEach, expect, test } from "bun:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"

import {
  initializeRequest,
  runAdapterProcess,
  shutdownRequest,
} from "./helpers"

afterEach(async () => {
  await cleanup()
})

const tempRoots: string[] = []

async function cleanup(): Promise<void> {
  for (const dir of tempRoots.splice(0)) {
    await rm(dir, { recursive: true, force: true })
  }
}

test("event notifications pass the raw hya envelope to event handlers", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "hya-bun-event-"))
  tempRoots.push(root)
  const markerFile = path.join(root, "event.json")
  const extensionFile = path.join(root, "event.ts")
  await writeFile(
    extensionFile,
    [
      'import { writeFile } from "node:fs/promises"',
      "export default {",
      '  id: "event",',
      "  server: async () => ({",
      "    event: async (envelope) => {",
      "      await writeFile(process.env.HYA_EVENT_MARKER, JSON.stringify(envelope))",
      "    },",
      "  }),",
      "}",
    ].join("\n"),
  )

  const envelope = {
    seq: 9,
    ts_millis: 12,
    event: {
      type: "tool_called",
      session: "session-1",
      tool: "read",
    },
  }

  const responses = await runAdapterProcess(
    [
      initializeRequest(1, {
        activation_id: "activation-event-test",
        lifecycle: "transient",
      }),
      {
        jsonrpc: "2.0",
        method: "event",
        params: { envelope },
      },
      shutdownRequest(2),
    ],
    {
      argv: ["--bundle-extension", extensionFile],
      env: { HYA_EVENT_MARKER: markerFile },
    },
  )

  expect(responses.map((response) => response.id)).toEqual([1, 2])
  const received = JSON.parse(await readFile(markerFile, "utf8")) as unknown
  expect(received).toEqual(envelope)
})
