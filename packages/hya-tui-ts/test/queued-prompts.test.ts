import { expect, test } from "bun:test"
import { readFile } from "node:fs/promises"
import path from "node:path"

import { CommandMap, Definitions } from "../src/upstream/config/keybind"
import {
  createQueuedPromptStore,
  resolvePromptDelivery,
} from "../src/hya/queued-prompts"

const repoRoot = path.resolve(import.meta.dir, "../../..")
const tuiRoot = path.resolve(import.meta.dir, "..")

test("idle submits send immediately for every intent", () => {
  expect(resolvePromptDelivery("idle", "submit")).toBe("send")
  expect(resolvePromptDelivery("idle", "steer")).toBe("send")
  expect(resolvePromptDelivery("idle", "queue")).toBe("send")
})

test("busy submit and queue stay queued while steer aborts the current turn", () => {
  expect(resolvePromptDelivery("busy", "submit")).toBe("queue")
  expect(resolvePromptDelivery("busy", "queue")).toBe("queue")
  expect(resolvePromptDelivery("busy", "steer")).toBe("steer")
})

test("queued prompt store is FIFO per session and can drop items before drain", () => {
  const store = createQueuedPromptStore({
    now: () => 1000,
    nextId: (() => {
      let n = 0
      return () => `q-${++n}`
    })(),
  })

  const first = store.enqueue({ sessionID: "s1", text: "one", parts: [] })
  const second = store.enqueue({ sessionID: "s1", text: "two", parts: [{ type: "file" }] })
  store.enqueue({ sessionID: "s2", text: "other", parts: [] })

  expect(first.id).toBe("q-1")
  expect(store.list("s1").map((item) => item.text)).toEqual(["one", "two"])
  expect(store.remove(second.id)?.text).toBe("two")
  expect(store.dequeue("s1")?.text).toBe("one")
  expect(store.dequeue("s1")).toBeUndefined()
  expect(store.list("s2")).toHaveLength(1)

  store.pauseDrain()
  expect(store.isDrainPaused()).toBe(true)
  store.resumeDrain()
  expect(store.isDrainPaused()).toBe(false)
})

test("TUI maps distinct steer and queue commands and registers queued-prompt handlers", async () => {
  expect(CommandMap.prompt_submit_steer).toBe("prompt.submit.steer")
  expect(CommandMap.prompt_submit_queue).toBe("prompt.submit.queue")
  expect(CommandMap.queued_prompt_delete).toBe("queued_prompt.delete")
  expect(Definitions.prompt_submit_steer.default).toBe("ctrl+alt+return")
  expect(Definitions.session_queued_prompts.default).toBe("<leader>q")

  const prompt = await readFile(path.join(tuiRoot, "src/upstream/component/prompt/index.tsx"), "utf8")
  expect(prompt).toContain('name: "prompt.submit.steer"')
  expect(prompt).toContain('name: "prompt.submit.queue"')
  expect(prompt).toContain('name: "session.queued_prompts"')
  expect(prompt).toContain("resolvePromptDelivery")

  const keyDocs = await readFile(path.join(repoRoot, "docs/tui-keybindings.md"), "utf8")
  expect(keyDocs).toContain("`prompt.submit.steer`")
  expect(keyDocs).not.toContain("no current command handler is registered")
})
