import { expect, test } from "bun:test"
import type { ToolPart } from "@opencode-ai/sdk/v2"

import { presentCodingTool } from "../src/hya/coding-tool-presentation"

type CompletedPartOptions = {
  tool: string
  input: Record<string, unknown>
  output?: string
  title?: string
  metadata?: Record<string, unknown>
}

/** Build a completed SDK ToolPart without introducing a transport or store seam. */
function completedPart({ tool, input, output = "", title = tool, metadata = {} }: CompletedPartOptions): ToolPart {
  return {
    id: `part-${tool}`,
    sessionID: "session-coding-tool",
    messageID: "message-coding-tool",
    type: "tool",
    callID: `call-${tool}`,
    tool,
    state: {
      status: "completed",
      input,
      output,
      title,
      metadata,
      time: { start: 1, end: 2 },
    },
  }
}

test("completed Read normalizes display text, path, and starting line", () => {
  const text = "const answer = 42\nreturn answer"
  const view = presentCodingTool(
    completedPart({
      tool: "read",
      input: { path: "src/main.ts", offset: 12, limit: 2 },
      output: "12: const answer = 42\n13: return answer",
      title: "Read src/main.ts",
      metadata: {
        truncated: false,
        display: {
          type: "file",
          path: "src/main.ts",
          text,
          lineStart: 12,
          truncated: false,
        },
      },
    }),
  )

  expect(view).toMatchObject({
    kind: "read-code",
    title: "Read src/main.ts",
    path: "src/main.ts",
    text,
    lineStart: 12,
    truncated: false,
  })
})

test("completed Edit normalizes its bounded diff and diagnostics", () => {
  const diff = "@@ -1 +1 @@\n-const oldValue = 1\n+const newValue = 2"
  const diagnostics = [{ message: "type check passed", severity: 1, range: { start: { line: 0, character: 0 } } }]
  const view = presentCodingTool(
    completedPart({
      tool: "edit",
      input: { path: "src/main.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/main.ts",
      metadata: { diff, diagnostics },
    }),
  )

  expect(view).toMatchObject({
    kind: "edit-diff",
    title: "Edit src/main.ts",
    path: "src/main.ts",
    diff,
    diagnostics,
  })
})

test("coding-tool views preserve each backend result-cap truncation marker", () => {
  const read = presentCodingTool(
    completedPart({
      tool: "read",
      input: { path: "src/read.ts" },
      output: "const read = true",
      title: "Read src/read.ts",
      metadata: {
        truncated: false,
        displayTruncated: true,
        display: {
          type: "file",
          path: "src/read.ts",
          text: "const read = true",
          lineStart: 1,
          truncated: false,
        },
      },
    }),
  )
  expect(read).toMatchObject({ kind: "read-code", truncated: true })

  const write = presentCodingTool(
    completedPart({
      tool: "write",
      input: { path: "src/write.ts", content: "const write = true" },
      output: "Wrote src/write.ts",
      title: "Write src/write.ts",
      metadata: {
        truncated: false,
        displayTruncated: true,
        display: {
          type: "file",
          path: "src/write.ts",
          text: "const write = true",
          lineStart: 1,
          truncated: false,
        },
      },
    }),
  )
  expect(write).toMatchObject({ kind: "write-code", truncated: true })

  for (const flag of ["displayTruncated", "rowsTruncated", "groupsTruncated"] as const) {
    const groups = flag === "groupsTruncated" ? [] : [{ path: "src/matches.ts", rows: [] }]
    const grep = presentCodingTool(
      completedPart({
        tool: "grep",
        input: { pattern: "needle", path: "src" },
        output: "",
        title: "Grep needle",
        metadata: {
          truncated: false,
          [flag]: true,
          display: { groups },
        },
      }),
    )
    expect(grep, `Grep ${flag}`).toMatchObject({ kind: "grep-output", truncated: true })
  }

  const edit = presentCodingTool(
    completedPart({
      tool: "edit",
      input: { path: "src/edit.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/edit.ts",
      metadata: { truncated: false, diffTruncated: true, diff: "@@ -1 +1 @@\n-old\n+new" },
    }),
  )
  expect(edit).toMatchObject({ kind: "edit-diff", truncated: true })
})
test("coding-tool views surface every remaining backend cap fact", () => {
  const globalFlags = [
    "outputTruncated",
    "titleTruncated",
    "attachmentsTruncated",
    "diagnosticsTruncated",
    "warningsTruncated",
    "metadataTruncated",
    "unknownFieldsDropped",
    "envelopeTruncated",
  ] as const

  for (const flag of globalFlags) {
    const view = presentCodingTool(
      completedPart({
        tool: "edit",
        input: { path: "src/edit.ts", edits: [{ type: "replace", line: 1 }] },
        output: "Edit applied successfully.",
        title: "Edit src/edit.ts",
        metadata: {
          [flag]: true,
          diff: "@@ -1 +1 @@\n-old\n+new",
          diagnostics: [],
        },
      }),
    )
    expect(view, `Edit ${flag}`).toMatchObject({ kind: "edit-diff", truncated: true })
  }
})

test("Bash timeout and signal results retain output when exit is nullable", () => {
  for (const termination of ["timeout", "signal"] as const) {
    const output = `${termination} output remains visible`
    const view = presentCodingTool(
      completedPart({
        tool: "bash",
        input: { command: `printf ${termination}` },
        output,
        title: `Bash ${termination}`,
        metadata: {
          exit: null,
          timedOut: true,
          signal: termination === "signal" ? "SIGTERM" : undefined,
        },
      }),
    )

    expect(view, `Bash ${termination}`).toMatchObject({
      kind: "shell-output",
      output,
      exit: undefined,
      timedOut: true,
      status: "Timed out",
    })
  }
})

test("completed diagnostics keep the first three positioned severity-one errors", () => {
  const diagnostics = [
    { message: "first error", severity: 1, range: { start: { line: 0, character: 1 } } },
    { message: "second error", severity: 1, range: { start: { line: 4, character: 2 } } },
    { message: "third error", severity: 1, range: { start: { line: 8, character: 3 } } },
    { message: "informational note", severity: 2, range: { start: { line: 12, character: 4 } } },
    { message: "missing location", severity: 1 },
    { message: "fourth error", severity: 1, range: { start: { line: 16, character: 5 } } },
  ]
  const view = presentCodingTool(
    completedPart({
      tool: "edit",
      input: { path: "src/diagnostics.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/diagnostics.ts",
      metadata: { diff: "@@ -1 +1 @@\n-old\n+new", diagnostics },
    }),
  )

  expect(view).toBeDefined()
  if (!view || view.kind !== "edit-diff") throw new Error("expected an edit view")
  expect(view.diagnostics).toEqual(diagnostics.slice(0, 3))
})
test("malformed diagnostic metadata selects the safe fallback instead of being silently skipped", () => {
  const view = presentCodingTool(
    completedPart({
      tool: "edit",
      input: { path: "src/diagnostics.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/diagnostics.ts",
      metadata: {
        diff: "@@ -1 +1 @@\n-old\n+new",
        diagnostics: [
          { message: "valid error", severity: 1, range: { start: { line: 0, character: 0 } } },
          { message: 42, severity: 1, range: { start: { line: 1, character: 1 } } },
        ],
      },
    }),
  )

  expect(view).toBeUndefined()
})

test("completed Grep keeps per-file groups and match identity", () => {
  const groups = [
    {
      path: "src/main.ts",
      rows: [
        { line: 4, text: "const needle = 1", isMatch: true },
        { line: 5, text: "return needle", isMatch: false },
      ],
    },
    {
      path: "src/other.ts",
      rows: [{ line: 9, text: "needle()", isMatch: true }],
    },
  ]
  const view = presentCodingTool(
    completedPart({
      tool: "grep",
      input: { pattern: "needle", path: "src" },
      output: "src/main.ts:4:const needle = 1\nsrc/other.ts:9:needle()",
      title: "Grep needle",
      metadata: {
        truncated: false,
        display: { groups },
      },
    }),
  )

  expect(view).toMatchObject({
    kind: "grep-output",
    title: "Grep needle",
    pattern: "needle",
    groups,
    output: "src/main.ts:4:const needle = 1\nsrc/other.ts:9:needle()",
    truncated: false,
  })
})

test("completed Write normalizes the requested path and content", () => {
  const content = "export const answer = 42\n"
  const view = presentCodingTool(
    completedPart({
      tool: "write",
      input: { path: "src/generated.ts", content },
      output: "Wrote src/generated.ts",
      title: "Write src/generated.ts",
      metadata: { diagnostics: [] },
    }),
  )

  expect(view).toMatchObject({
    kind: "write-code",
    title: "Write src/generated.ts",
    path: "src/generated.ts",
    text: content,
    diagnostics: [],
  })
})

test("bash and its hidden shell alias share the shell-output view", () => {
  for (const tool of ["bash", "shell"] as const) {
    const view = presentCodingTool(
      completedPart({
        tool,
        input: { command: "printf coding-tool", cwd: "/work", env: { TOKEN: "secret-value" } },
        output: "coding-tool",
        title: "Bash",
        metadata: { exit: 0, truncated: false },
      }),
    )

    expect(view).toMatchObject({
      kind: "shell-output",
      title: "Bash",
      command: "printf coding-tool",
      cwd: "/work",
      output: "coding-tool",
      exit: 0,
      truncated: false,
    })
  }
})

test("shell output strips ANSI control sequences before presentation", () => {
  const view = presentCodingTool(
    completedPart({
      tool: "bash",
      input: { command: "printf colored" },
      output: "\u001b[31mcolored\u001b[0m\n",
      title: "Bash",
      metadata: { exit: 0, truncated: false },
    }),
  )

  expect(view).toMatchObject({ kind: "shell-output", output: "colored\n" })
})

test("shell presentation excludes environment names and values", () => {
  const view = presentCodingTool(
    completedPart({
      tool: "bash",
      input: {
        command: "printf safe",
        env: { API_TOKEN: "super-secret-token" },
      },
      output: "safe",
      title: "Bash",
      metadata: { exit: 0, unknown: "internal-only", secret: "super-secret-token" },
    }),
  )

  expect(view).toBeDefined()
  expect(view).not.toHaveProperty("env")
  expect(JSON.stringify(view)).not.toContain("API_TOKEN")
  expect(JSON.stringify(view)).not.toContain("internal-only")
  expect(JSON.stringify(view)).not.toContain("secret")
  expect(JSON.stringify(view)).not.toContain("super-secret-token")
})

test("malformed completed metadata returns the existing fallback signal", () => {
  const view = presentCodingTool(
    completedPart({
      tool: "read",
      input: { path: "src/broken.ts" },
      output: "not a display payload",
      title: "Read src/broken.ts",
      metadata: {
        display: {
          type: "file",
          path: 42,
          text: null,
          lineStart: "first",
        },
      },
    }),
  )

  expect(view).toBeUndefined()
})
