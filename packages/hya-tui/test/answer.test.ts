import { expect, test } from "bun:test"
import type { HyaClient, Interaction } from "../src/client"
import { answerPrompt } from "../src/app/prompts"
import { createAppStore } from "../src/state/store"

const perm: Interaction = { id: "perm_1", session: "hysec_1", type: "INTERACTION_TYPE_PERMISSION", title: "bash ls", payload: { action: "bash", resource: "ls" } }
const ask: Interaction = { id: "que_1", session: "hysec_1", type: "INTERACTION_TYPE_QUESTION", title: "Color?", options: ["red"] }

function harness(respond: (id: string, body: unknown) => Promise<unknown>) {
  const store = createAppStore()
  store.openSession({ id: "hysec_1", agent: "build", workdir: "/w" })
  store.setInteractions([perm, ask])
  const sent: [string, unknown][] = []
  let listed = 0
  const client = {
    respondInteraction: (id: string, body: unknown) => {
      sent.push([id, body])
      return respond(id, body)
    },
    listInteractions: async () => {
      listed++
      return [perm, ask]
    },
  } as unknown as HyaClient
  return { store, client, sent, listed: () => listed }
}

test("a choice sends its respond body, hides the ask at once, and says what was decided", async () => {
  const { store, client, sent } = harness(async () => ({ applied: true }))
  const done = answerPrompt({ store, client }, perm, { kind: "allowAlways" })
  expect(store.state.interactions.map((row) => row.id)).toEqual(["que_1"])
  await done
  expect(sent).toEqual([["perm_1", { permission: { allowed: true, persist: true } }]])
  expect(store.state.status).toBe("Always allowed · bash ls")
  await answerPrompt({ store, client }, ask, { kind: "answer", answer: "red" })
  expect(sent.at(-1)).toEqual(["que_1", { question: { answer: "red" } }])
  expect(store.state.status).toBe("Answered · red")
})

test("deny, reject, and allow once report their own status; Other… only points at the input", async () => {
  const { store, client, sent } = harness(async () => ({ applied: true }))
  await answerPrompt({ store, client }, ask, { kind: "other" })
  expect(sent).toEqual([])
  expect(store.state.status).toBe("Type the answer in the input · Enter sends it")
  await answerPrompt({ store, client }, ask, { kind: "reject" })
  expect(store.state.status).toBe("Rejected the question")
  await answerPrompt({ store, client }, perm, { kind: "deny" })
  expect(store.state.status).toBe("Denied · bash ls")
})

test("an answer already resolved elsewhere, or a failed one, is reported; a failed ask shows again", async () => {
  const stale = harness(async () => ({ applied: false }))
  await answerPrompt(stale, perm, { kind: "allowOnce" })
  expect(stale.store.state.status).toBe("Already answered elsewhere · bash ls")
  const failing = harness(async () => { throw new Error("boom") })
  await answerPrompt(failing, perm, { kind: "allowOnce" })
  expect(failing.store.state.status).toBe("Answer failed: Error: boom")
  expect(failing.listed()).toBe(1)
  expect(failing.store.state.interactions.map((row) => row.id)).toEqual(["perm_1", "que_1"])
})
