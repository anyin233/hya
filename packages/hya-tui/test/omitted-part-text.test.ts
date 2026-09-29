import { expect, test } from "bun:test"
import type { MessageInfo } from "../src/client"
import { childActivity } from "../src/state/members"
import { messageView } from "../src/state/messages"

// protojson omits empty strings: a thinking block whose text is empty (for
// example a signature-only one) arrives as `reasoning: {}`, and an empty text
// part as `text: {}`. Rendering them must not throw, or every refresh of the
// session fails ("Refresh failed: … text.split").
const assistant: MessageInfo = {
  id: "a",
  role: "ROLE_ASSISTANT",
  finish: "FINISH_REASON_STOP",
  parts: [
    { id: "r", reasoning: {} },
    { id: "t", text: {} },
    { id: "d", text: { text: "Done." } },
  ],
}

test("a reasoning part without text renders as an empty thinking block", () => {
  const view = messageView(assistant, { agent: "hya-main", model: "fake/model" })
  expect(view.blocks).toEqual([
    { kind: "reasoning", id: "r", text: "", words: 0, active: false },
    { kind: "text", id: "d", text: "Done." },
  ])
})

test("a child's activity skips text parts without text", () => {
  expect(childActivity([{ ...assistant, parts: [{ id: "d", text: { text: "Done." } }, { id: "t", text: {} }] }])).toBe("Done.")
})
