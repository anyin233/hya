import { expect, test } from "bun:test"
import { clipLines, diffLines, formatDuration, partialField, toolBodyLines, toolCard, toolStatus, type ToolLine } from "../src/state/tools"

const ok = "TOOL_EXECUTION_STATE_OK"
const json = (value: unknown) => JSON.stringify(value)
const texts = (lines: ToolLine[]) => lines.map((line) => line.text)

test("tool states map to pending, running, done, failed", () => {
  expect(toolStatus("TOOL_EXECUTION_STATE_PENDING")).toBe("pending")
  expect(toolStatus("TOOL_EXECUTION_STATE_RUNNING")).toBe("running")
  expect(toolStatus(ok)).toBe("done")
  expect(toolStatus("TOOL_EXECUTION_STATE_ERROR")).toBe("failed")
  expect(toolStatus(undefined)).toBe("pending")
})

test("durations are short and human", () => {
  expect(formatDuration(0)).toBe("0ms")
  expect(formatDuration(42)).toBe("42ms")
  expect(formatDuration(1500)).toBe("1.5s")
  expect(formatDuration(12_000)).toBe("12s")
  expect(formatDuration(65_000)).toBe("1m 5s")
})

test("long bodies keep the head and the tail around a hidden-lines marker", () => {
  const lines = Array.from({ length: 30 }, (_, index) => ({ text: `line ${index + 1}`, tone: "muted" as const }))
  const clipped = clipLines(lines)
  expect(clipped).toHaveLength(toolBodyLines)
  expect(clipped[0]!.text).toBe("line 1")
  expect(clipped.at(-1)!.text).toBe("line 30")
  expect(clipped.find((line) => line.text.startsWith("…"))).toEqual({ text: "… 19 lines hidden", tone: "muted" })
  expect(clipLines(lines.slice(0, 5))).toHaveLength(5)
})

test("bash: the command, exit status, duration, and output tail", () => {
  const card = toolCard({
    tool: "bash", state: ok, durationMs: "1500",
    inputJson: json({ command: "make test" }),
    outputJson: json({ title: "make test", output: `${Array.from({ length: 20 }, (_, i) => `out ${i + 1}`).join("\n")}\n`, metadata: { exit: 2 } }),
  })
  expect(card).toMatchObject({ status: "done", tool: "bash", summary: "make test · exit 2", duration: "1.5s" })
  expect(card.body[0]).toEqual({ text: "$ make test", tone: "fg" })
  expect(card.body.at(-1)).toEqual({ text: "exit 2", tone: "error" })
  expect(texts(card.body)).toContain("out 20")
  expect(texts(card.body).some((text) => /lines hidden/.test(text))).toBe(true)
  const clean = toolCard({ tool: "bash", state: ok, inputJson: json({ command: "echo hi" }), outputJson: json({ output: "hi\n", metadata: { exit: 0 } }) })
  expect(clean.summary).toBe("echo hi")
  expect(clean.body).toEqual([{ text: "$ echo hi", tone: "fg" }, { text: "hi", tone: "muted" }])
})

test("bash: a shell turn's command is used when the part has no input yet", () => {
  expect(toolCard({ tool: "bash", state: "TOOL_EXECUTION_STATE_RUNNING" }, { command: "sleep 5" })).toMatchObject({ status: "running", summary: "sleep 5", command: "sleep 5" })
})

test("read: the path and the line range", () => {
  const card = toolCard({
    tool: "read", state: ok, durationMs: "3",
    inputJson: json({ path: "src/a.ts", offset: 2, limit: 2 }),
    outputJson: json({ content: "two\nthree", metadata: { display: { type: "file", path: "/w/src/a.ts", text: "two\nthree", lineStart: 2, lineEnd: 3, totalLines: 4 } } }),
  })
  expect(card).toMatchObject({ summary: "src/a.ts · lines 2-3 of 4", duration: "3ms" })
  expect(texts(card.body)).toEqual(["2  two", "3  three"])
  expect(toolCard({ tool: "read", state: "TOOL_EXECUTION_STATE_RUNNING", inputJson: json({ path: "a.txt", offset: 10 }) }).summary).toBe("a.txt · from line 10")
  expect(toolCard({ tool: "read", state: "TOOL_EXECUTION_STATE_RUNNING", inputJson: json({ path: "a.txt" }) }).summary).toBe("a.txt")
})

test("edit: the path and a colored diff from the output's unified diff", () => {
  const diff = "--- /w/a.txt\n+++ /w/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"
  const card = toolCard({
    tool: "edit", state: ok,
    inputJson: json({ path: "a.txt", edits: [{ op: "replace_text", oldText: "two", newText: "TWO" }] }),
    outputJson: json({ output: "Edit applied successfully.", metadata: { diff } }),
  })
  expect(card.summary).toBe("a.txt · +1 -1")
  expect(card.body).toEqual([
    { text: "@@ -1,3 +1,3 @@", tone: "hunk" },
    { text: "  one", tone: "muted" },
    { text: "- two", tone: "remove" },
    { text: "+ TWO", tone: "add" },
    { text: "  three", tone: "muted" },
  ])
})

test("edit: without output the diff is derived from the edit arguments", () => {
  const card = toolCard({
    tool: "edit", state: "TOOL_EXECUTION_STATE_RUNNING",
    inputJson: json({ path: "a.txt", edits: [{ op: "replace_text", oldText: "a\nb", newText: "c" }, { op: "append", lines: ["tail"] }] }),
  })
  expect(card.summary).toBe("a.txt · +2 -2")
  expect(card.body).toEqual([
    { text: "- a", tone: "remove" }, { text: "- b", tone: "remove" }, { text: "+ c", tone: "add" }, { text: "+ tail", tone: "add" },
  ])
  // Compat edit shapes (MCP / other harnesses) use old/new string fields.
  const compat = toolCard({ tool: "edit", state: ok, inputJson: json({ filePath: "b.ts", oldString: "x", newString: "y" }) })
  expect(compat.summary).toBe("b.ts · +1 -1")
})

test("write: the path, the line count, and the content as additions", () => {
  const card = toolCard({ tool: "write", state: ok, inputJson: json({ path: "b.txt", content: "new\nfile\n" }) })
  expect(card.summary).toBe("b.txt · 2 lines")
  expect(card.body).toEqual([{ text: "+ new", tone: "add" }, { text: "+ file", tone: "add" }])
})

test("apply_patch: the files and the patch hunks", () => {
  const patchText = "*** Begin Patch\n*** Update File: src/a.rs\n@@ fn main\n-old\n+new\n ctx\n*** Add File: b.txt\n+hello\n*** End Patch"
  const card = toolCard({ tool: "apply_patch", state: ok, inputJson: json({ patchText }) })
  expect(card.summary).toBe("src/a.rs, b.txt · +2 -1")
  expect(card.body).toEqual([
    { text: "src/a.rs", tone: "hunk" },
    { text: "@@ fn main", tone: "hunk" },
    { text: "- old", tone: "remove" },
    { text: "+ new", tone: "add" },
    { text: "  ctx", tone: "muted" },
    { text: "b.txt (new)", tone: "hunk" },
    { text: "+ hello", tone: "add" },
  ])
})

test("grep, glob, and find: the pattern, the scope, and the match count", () => {
  const grep = toolCard({
    tool: "grep", state: ok, inputJson: json({ pattern: "TODO", path: "src" }),
    outputJson: json({ total: 2, matches: [{ file: "a.rs", line: 1, text: "// TODO one" }, { file: "b.rs", line: 9, text: "// TODO two" }], metadata: { matches: 2 } }),
  })
  expect(grep.summary).toBe("\"TODO\" in src · 2 matches")
  expect(texts(grep.body)).toEqual(["a.rs:1: // TODO one", "b.rs:9: // TODO two"])
  const glob = toolCard({ tool: "glob", state: ok, inputJson: json({ pattern: "*.txt" }), outputJson: json({ paths: ["a.txt", "b.txt"], total: 2, metadata: { count: 2 } }) })
  expect(glob.summary).toBe("*.txt · 2 files")
  expect(texts(glob.body)).toEqual(["a.txt", "b.txt"])
  expect(toolCard({ tool: "find", state: ok, inputJson: json({ pattern: "*.md", path: "docs" }), outputJson: json({ paths: ["x.md"] }) }).summary).toBe("*.md in docs · 1 file")
  expect(toolCard({ tool: "grep", state: ok, inputJson: json({ pattern: "x" }), outputJson: json({ total: 1, matches: [{ file: "a", line: 1, text: "x" }] }) }).summary).toBe("\"x\" · 1 match")
})

test("todo tools list the todos with their status", () => {
  const card = toolCard({
    tool: "todo__update_status", state: ok, inputJson: json({ updates: [{ id: "1", status: "completed" }] }),
    outputJson: json({ metadata: { todos: [{ id: "1", content: "write tests", status: "completed" }, { id: "2", content: "ship", status: "in_progress" }] } }),
  })
  expect(card.summary).toBe("2 todos · 1 done")
  expect(texts(card.body)).toEqual(["✓ write tests", "▸ ship"])
})

test("network, skill, and question tools show their main argument", () => {
  expect(toolCard({ tool: "webfetch", state: ok, inputJson: json({ url: "https://example.com", format: "markdown" }) }).summary).toBe("https://example.com")
  expect(toolCard({ tool: "websearch", state: ok, inputJson: json({ query: "bun pty" }) }).summary).toBe("\"bun pty\"")
  expect(toolCard({ tool: "skill", state: ok, inputJson: json({ name: "release-notes" }) }).summary).toBe("release-notes")
  expect(toolCard({ tool: "ask_user", state: ok, inputJson: json({ questions: [{ header: "Scope", question: "Which crate?", options: [] }] }) }).summary).toBe("Scope: Which crate?")
})

test("task: the child agent, description, and the child session from the output", () => {
  const card = toolCard({
    tool: "task", state: ok, callId: "call_1",
    inputJson: json({ description: "survey the repo", prompt: "list files", subagent_type: "scout" }),
    outputJson: json({ title: "survey the repo", metadata: { sessionId: "hysec_child", parentSessionId: "hysec_p", subagent_type: "scout", status: "running" }, output: "…" }),
  })
  expect(card.summary).toBe("scout · survey the repo")
  expect(card.task).toEqual({ agent: "scout", description: "survey the repo", child: "hysec_child" })
  expect(toolCard({ tool: "task", state: "TOOL_EXECUTION_STATE_RUNNING", inputJson: json({ description: "x", prompt: "p" }) }).task).toEqual({ agent: "general", description: "x" })
})

test("MCP and other tools show the name and compact arguments", () => {
  const card = toolCard({ tool: "github__search_issues", state: ok, inputJson: json({ repo: "a/b", query: "bug" }), outputJson: json("3 issues") })
  expect(card.summary).toBe("{\"repo\":\"a/b\",\"query\":\"bug\"}")
  expect(texts(card.body)).toEqual(["3 issues"])
})

test("a failed call carries its error message", () => {
  const card = toolCard({ tool: "read", state: "TOOL_EXECUTION_STATE_ERROR", inputJson: json({ path: "missing.txt" }), errorCode: "unknown", errorMessage: "File not found: /w/missing.txt" })
  expect(card).toMatchObject({ status: "failed", summary: "missing.txt", error: "File not found: /w/missing.txt" })
  expect(toolCard({ tool: "x", state: "TOOL_EXECUTION_STATE_ERROR" }).error).toBe("failed")
})

test("arguments still streaming: the main field is read from the partial JSON", () => {
  expect(partialField("{\"command\":\"ls -la /tm", "command")).toBe("ls -la /tm")
  expect(partialField("{\"path\":\"a\\\"b\",", "path")).toBe("a\"b")
  expect(partialField("{\"pa", "path")).toBeUndefined()
  expect(toolCard({ tool: "bash", state: "TOOL_EXECUTION_STATE_PENDING", inputJson: "{\"command\":\"cargo te" })).toMatchObject({ status: "pending", summary: "cargo te" })
})

test("diff lines skip file headers and tone each row", () => {
  expect(diffLines("--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\n")).toEqual([
    { text: "@@ -1 +1 @@", tone: "hunk" }, { text: "- x", tone: "remove" }, { text: "+ y", tone: "add" },
  ])
})
