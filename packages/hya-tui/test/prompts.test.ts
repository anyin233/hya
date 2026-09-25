import { expect, test } from "bun:test"
import type { Interaction, MemberInfo, MessageInfo, SessionInfo } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import {
  currentPrompt,
  mergeInteractions,
  promptKey,
  promptQueue,
  promptView,
  respondBody,
  treeSessionIds,
  waitingKind,
  type PromptContext,
} from "../src/state/prompts"

const permission = (id: string, session: string, payload: Record<string, unknown>, title = ""): Interaction => ({
  id, session, type: "INTERACTION_TYPE_PERMISSION", title, payload,
})
const question = (id: string, session: string, title: string, options: string[] = [], detail = ""): Interaction => ({
  id, session, type: "INTERACTION_TYPE_QUESTION", title, options, ...(detail ? { detail } : {}),
})
const session = (id: string, extra: Partial<SessionInfo> = {}): SessionInfo => ({ id, agent: "build", workdir: "/w", ...extra })
const context = (extra: Partial<PromptContext> = {}): PromptContext => ({
  selected: session("hysec_p"),
  sessions: [session("hysec_p")],
  members: [],
  messages: [],
  ...extra,
})
const key = (name: string, extra: Partial<KeyLike> = {}): KeyLike => ({ name, ctrl: false, meta: false, shift: false, sequence: name.length === 1 ? name : "", ...extra })
const texts = (lines: { text: string }[]) => lines.map((line) => line.text)

test("bash asks show the command; options are allow once, always allow (with the saved patterns), and deny", () => {
  const view = promptView(permission("perm_1", "hysec_p", {
    action: "bash", resource: "echo hello", always: ["echo hello *"], callId: "call_1", tool: "bash", input: { command: "echo hello" },
  }, "bash echo hello"), context(), 0, 1)
  expect(view).toMatchObject({ id: "perm_1", kind: "permission", title: "bash echo hello", asker: "build", subagent: false, tool: "bash", position: 0, total: 1 })
  expect(texts(view.body)).toEqual(["$ echo hello"])
  // The headline reads like the tool card: tool and summary.
  expect(view.headline).toBe("bash  echo hello")
  expect(view.options.map((option) => option.label)).toEqual(["Allow once", "Always allow", "Deny"])
  expect(view.options[1]!.detail).toBe("bash: echo hello *")
  expect(view.options.map((option) => option.choice.kind)).toEqual(["allowOnce", "allowAlways", "deny"])
})

test("edit asks show the path and a colored diff; write asks show the new content", () => {
  const edit = promptView(permission("perm_e", "hysec_p", {
    action: "edit", resource: "src/a.ts", tool: "edit",
    input: { path: "src/a.ts", edits: [{ oldText: "old line", newText: "new line" }] },
  }, "edit src/a.ts"), context(), 0, 1)
  expect(edit.summary).toBe("src/a.ts · +1 -1")
  expect(edit.headline).toBe("edit  src/a.ts · +1 -1")
  expect(edit.body).toEqual([{ text: "- old line", tone: "remove" }, { text: "+ new line", tone: "add" }])
  const write = promptView(permission("perm_w", "hysec_p", {
    action: "edit", resource: "notes.md", tool: "write", input: { path: "notes.md", content: "a\nb" },
  }, "edit notes.md"), context(), 0, 1)
  expect(write.summary).toBe("notes.md · 2 lines")
  expect(write.body.map((line) => line.tone)).toEqual(["add", "add"])
})

test("read/webfetch asks show the path or url; generic tools show compact arguments; uncorrelated asks the resource", () => {
  const fetch = promptView(permission("perm_f", "hysec_p", { action: "webfetch", resource: "https://x.dev", tool: "webfetch", input: { url: "https://x.dev" } }), context(), 0, 1)
  expect(texts(fetch.body)).toEqual(["https://x.dev"])
  // Struct numbers arrive as doubles; compact JSON keeps them readable.
  const generic = promptView(permission("perm_g", "hysec_p", { action: "tool", resource: "mcp__gh__issue", tool: "mcp__gh__issue", input: { repo: "a/b", number: 7 } }), context(), 0, 1)
  expect(texts(generic.body)).toEqual(['{"repo":"a/b","number":7}'])
  const bare = promptView(permission("perm_x", "hysec_p", { action: "external_directory", resource: "/etc/*" }, "external_directory /etc/*"), context(), 0, 1)
  expect(texts(bare.body)).toEqual(["/etc/*"])
  expect(bare.headline).toBe("external_directory /etc/*")
  expect(bare.options[1]!.detail).toBe("external_directory: /etc/*")
})

test("long details are clipped for the prompt", () => {
  const content = Array.from({ length: 40 }, (_, index) => `row ${index}`).join("\n")
  const view = promptView(permission("perm_w", "hysec_p", { action: "edit", resource: "big", tool: "write", input: { path: "big", content } }), context(), 0, 1)
  expect(view.body.length).toBeLessThanOrEqual(8)
  expect(view.body.some((line) => line.text.includes("lines hidden"))).toBe(true)
})

test("option keys: digits choose at once, arrows move, Enter chooses the highlighted option, Esc denies", () => {
  const view = promptView(permission("perm_1", "hysec_p", { action: "bash", resource: "ls", tool: "bash", input: { command: "ls" } }), context(), 0, 1)
  const empty = { index: 0, draft: "" }
  expect(promptKey(view, empty, key("1"))).toEqual({ type: "choose", choice: { kind: "allowOnce" } })
  expect(promptKey(view, empty, key("2"))).toEqual({ type: "choose", choice: { kind: "allowAlways" } })
  expect(promptKey(view, empty, key("3"))).toEqual({ type: "choose", choice: { kind: "deny" } })
  expect(promptKey(view, empty, key("4"))).toEqual({ type: "none" })
  expect(promptKey(view, empty, key("down"))).toEqual({ type: "move", index: 1 })
  expect(promptKey(view, empty, key("up"))).toEqual({ type: "move", index: 2 })
  expect(promptKey(view, { index: 2, draft: "" }, key("return"))).toEqual({ type: "choose", choice: { kind: "deny" } })
  expect(promptKey(view, empty, key("escape"))).toEqual({ type: "choose", choice: { kind: "deny" } })
  // Modified keys are never prompt keys.
  expect(promptKey(view, empty, key("c", { ctrl: true }))).toEqual({ type: "none" })
  expect(promptKey(view, empty, key("return", { meta: true }))).toEqual({ type: "none" })
})

test("with text in the input, keys go to the input: typing can never answer a permission prompt", () => {
  const view = promptView(permission("perm_1", "hysec_p", { action: "bash", resource: "ls" }), context(), 0, 1)
  const draft = { index: 0, draft: "run version 2" }
  for (const name of ["1", "2", "3", "return", "escape", "up", "down"]) expect(promptKey(view, draft, key(name))).toEqual({ type: "none" })
})

test("respond bodies: once, always (persist), deny, answer, reject", () => {
  expect(respondBody({ kind: "allowOnce" })).toEqual({ permission: { allowed: true, persist: false } })
  expect(respondBody({ kind: "allowAlways" })).toEqual({ permission: { allowed: true, persist: true } })
  expect(respondBody({ kind: "deny" })).toEqual({ permission: { allowed: false, persist: false } })
  expect(respondBody({ kind: "answer", answer: "blue" })).toEqual({ question: { answer: "blue" } })
  expect(respondBody({ kind: "reject" })).toEqual({ question: { rejected: true } })
  expect(respondBody({ kind: "other" })).toBeUndefined()
})

test("question prompts list the options, then Other… and Reject; free text in the input answers on Enter", () => {
  const view = promptView(question("que_1", "hysec_p", "Which color?", ["red", "blue"], "Color"), context(), 0, 1)
  expect(view).toMatchObject({ kind: "question", title: "Which color?", header: "Color", headline: "Color: Which color?", asker: "build" })
  expect(view.options.map((option) => option.label)).toEqual(["red", "blue", "Other…", "Reject"])
  const empty = { index: 0, draft: "" }
  expect(promptKey(view, empty, key("2"))).toEqual({ type: "choose", choice: { kind: "answer", answer: "blue" } })
  expect(promptKey(view, empty, key("3"))).toEqual({ type: "choose", choice: { kind: "other" } })
  expect(promptKey(view, empty, key("4"))).toEqual({ type: "choose", choice: { kind: "reject" } })
  expect(promptKey(view, empty, key("escape"))).toEqual({ type: "choose", choice: { kind: "reject" } })
  expect(promptKey(view, { index: 1, draft: "" }, key("return"))).toEqual({ type: "choose", choice: { kind: "answer", answer: "blue" } })
  // Typed text answers the question; a /command still runs as a command.
  expect(promptKey(view, { index: 0, draft: "  green  " }, key("return"))).toEqual({ type: "choose", choice: { kind: "answer", answer: "green" } })
  expect(promptKey(view, { index: 0, draft: "/deny que_1" }, key("return"))).toEqual({ type: "none" })
  expect(promptKey(view, { index: 0, draft: "green" }, key("2"))).toEqual({ type: "none" })
  // Without options: free text or reject.
  const free = promptView(question("que_2", "hysec_p", "Your name?"), context(), 0, 1)
  expect(free.options.map((option) => option.label)).toEqual(["Other…", "Reject"])
})

test("a listed question without options takes them from its waiting ask_user call", () => {
  const messages: MessageInfo[] = [{
    id: "msg_a", role: "ROLE_ASSISTANT", parts: [{
      id: "p1",
      toolCall: {
        tool: "ask_user", callId: "call_q", state: "TOOL_EXECUTION_STATE_RUNNING",
        inputJson: JSON.stringify({ questions: [{ header: "Color", question: "Which color?", options: [{ label: "red", description: "" }, { label: "blue", description: "" }] }] }),
      },
    }],
  }]
  const view = promptView(question("que_1", "hysec_p", "Which color?"), context({ messages }), 0, 1)
  expect(view.header).toBe("Color")
  expect(view.options.map((option) => option.label)).toEqual(["red", "blue", "Other…", "Reject"])
})

test("the queue holds the asks of the open session and its subagent sessions, oldest first", () => {
  const sessions = [
    session("hysec_p"),
    session("hysec_c", { parent: "hysec_p", agent: "general" }),
    session("hysec_g", { parent: "hysec_c", agent: "explore" }),
    session("hysec_other"),
  ]
  const members: MemberInfo[] = [{ member: "mbr_1", child: "hysec_new", agent: "scout", description: "survey" }]
  const tree = treeSessionIds("hysec_p", sessions, ["hysec_new"])
  expect([...tree].sort()).toEqual(["hysec_c", "hysec_g", "hysec_new", "hysec_p"])
  const interactions = [
    permission("perm_other", "hysec_other", { action: "bash", resource: "x" }),
    permission("perm_c", "hysec_c", { action: "bash", resource: "c" }),
    question("que_p", "hysec_p", "Why?"),
    permission("perm_new", "hysec_new", { action: "bash", resource: "n" }),
  ]
  const queue = promptQueue(interactions, context({ sessions, members }))
  expect(queue.map((item) => item.id)).toEqual(["perm_c", "que_p", "perm_new"])
  // In a subagent's view only that subtree asks.
  expect(promptQueue(interactions, context({ selected: sessions[1], sessions })).map((item) => item.id)).toEqual(["perm_c"])
  expect(promptQueue(interactions, context({ selected: undefined, sessions }))).toEqual([])
})

test("a subagent's ask is labelled with the subagent and its task", () => {
  const sessions = [session("hysec_p"), session("hysec_c", { parent: "hysec_p", agent: "general" })]
  const members: MemberInfo[] = [{ member: "mbr_1", child: "hysec_c", agent: "general", description: "survey the repo" }]
  const view = promptView(permission("perm_c", "hysec_c", { action: "bash", resource: "ls", tool: "bash", input: { command: "ls" } }), context({ sessions, members }), 1, 2)
  expect(view).toMatchObject({ asker: "subagent general · survey the repo", subagent: true, position: 1, total: 2 })
  // Before the session list knows the child: the member row still names it.
  const early = promptView(permission("perm_c", "hysec_c", { action: "bash", resource: "ls" }), context({ members }), 0, 1)
  expect(early.asker).toBe("subagent general · survey the repo")
})

test("listed rows keep the options and header a live frame carried; resolved ids stay hidden", () => {
  const live = question("que_1", "hysec_p", "Which color?", ["red", "blue"], "Color")
  const listed = [question("que_1", "hysec_p", "Which color?"), permission("perm_1", "hysec_p", { action: "bash" }), permission("perm_2", "hysec_p", { action: "bash" })]
  const merged = mergeInteractions(listed, new Map([["que_1", live]]), new Set(["perm_2"]))
  expect(merged.map((item) => item.id)).toEqual(["que_1", "perm_1"])
  expect(merged[0]).toMatchObject({ options: ["red", "blue"], detail: "Color" })
})

test("a session waits when one of its asks is pending: approval for a permission, an answer for a question", () => {
  const interactions = [permission("perm_c", "hysec_c", {}), question("que_d", "hysec_d", "Why?")]
  expect(waitingKind(interactions, "hysec_c")).toBe("approval")
  expect(waitingKind(interactions, "hysec_d")).toBe("answer")
  expect(waitingKind(interactions, "hysec_e")).toBeUndefined()
})

test("the shown prompt is the oldest ask of the tree, with its place in the queue", () => {
  const interactions = [permission("perm_1", "hysec_p", { action: "bash", resource: "a" }), permission("perm_2", "hysec_p", { action: "bash", resource: "b" })]
  const shown = currentPrompt({ ...context(), interactions })
  expect(shown?.interaction.id).toBe("perm_1")
  expect(shown?.view).toMatchObject({ position: 0, total: 2 })
  expect(currentPrompt({ ...context(), interactions: [] })).toBeUndefined()
})
