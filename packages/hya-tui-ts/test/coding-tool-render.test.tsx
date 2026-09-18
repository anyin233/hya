import { expect, test } from "bun:test"
import type { CapturedFrame, CapturedSpan } from "@opentui/core"
import type { ToolPart } from "@opencode-ai/sdk/v2"
import { testRender } from "@opentui/solid"

import { CodingToolPresentation } from "../src/hya/coding-tool-presentation"
import { toolCardTitle } from "../src/hya/tool-card"

type CompletedPartOptions = {
  tool: string
  input: Record<string, unknown>
  output?: string
  title?: string
  metadata?: Record<string, unknown>
}

/** Build one completed part for the renderer without coupling the test to transport state. */
function completedPart({ tool, input, output = "", title = tool, metadata = {} }: CompletedPartOptions): ToolPart {
  return {
    id: `render-part-${tool}`,
    sessionID: "session-coding-tool-render",
    messageID: "message-coding-tool-render",
    type: "tool",
    callID: `render-call-${tool}`,
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

/** Flatten semantic terminal spans into text while retaining line boundaries. */
function frameText(frame: CapturedFrame): string {
  const lines = frame.lines.map((line) => line.spans.map((span) => span.text).join(""))
  return lines.join("\n")
}

/** Return the framed title-bar line of a rendered card. */
function cardTitle(text: string): string {
  return text.split("\n").find((line) => line.includes("╭")) ?? ""
}

/** Find the span carrying one source token in a captured terminal frame. */
function spanContaining(frame: CapturedFrame, token: string): CapturedSpan | undefined {
  const spans = frame.lines.flatMap((line) => line.spans)
  return spans.find((span) => span.text.includes(token))
}

/** Find the span carrying one source token below the card title bar. */
function bodySpanContaining(frame: CapturedFrame, token: string): CapturedSpan | undefined {
  const body = frame.lines.filter((line) => !line.spans.some((span) => span.text.includes("╭")))
  return body.flatMap((line) => line.spans).find((span) => span.text.includes(token))
}

/** Render a completed coding-tool part at the requested terminal width. */
async function renderPart(part: ToolPart, width: number) {
  const title = toolCardTitle(part.tool, part.state.status === "pending" ? undefined : part.state.input)
  return testRender(
    () => <CodingToolPresentation part={part} title={title} width={width} diffStyle="auto" diffWrapMode="none" />,
    { width, height: 24, footerHeight: 0 },
  )
}

test("Read cards title-bar the call and keep source, line offset, and syntax spans at 80 and 140 columns", async () => {
  const part = completedPart({
    tool: "read",
    input: { path: "src/main.ts", offset: 12, limit: 2 },
    output: "12: const answer = 42\n13: return answer",
    title: "Read src/main.ts",
    metadata: {
      display: {
        type: "file",
        path: "src/main.ts",
        text: "const answer = 42\nreturn answer",
        lineStart: 12,
        truncated: false,
      },
    },
  })

  for (const width of [80, 140]) {
    const setup = await renderPart(part, width)
    try {
      await setup.waitForFrame((frame) => frame.includes("const answer"))
      let syntaxReady = false
      for (let attempt = 0; attempt < 200 && !syntaxReady; attempt += 1) {
        await new Promise<void>((resolve) => setTimeout(resolve, 10))
        const pending = setup.captureSpans()
        const keyword = bodySpanContaining(pending, "const")
        const number = bodySpanContaining(pending, "42")
        syntaxReady = keyword !== undefined && number !== undefined && keyword.fg.toString() !== number.fg.toString()
      }
      expect(syntaxReady).toBe(true)
      const captured = setup.captureSpans()
      const text = frameText(captured)
      expect(cardTitle(text)).toContain("read [path=src/main.ts, offset=12, limit=2]")

      const sourceLine = text.split("\n").find((line) => line.includes("const answer"))
      expect(sourceLine).toBeDefined()
      expect(sourceLine).toMatch(/(?:^|\D)12(?:\D|$)/)

      const keyword = bodySpanContaining(captured, "const")
      const number = bodySpanContaining(captured, "42")
      expect(keyword).toBeDefined()
      expect(number).toBeDefined()
      expect(keyword?.fg.toString()).not.toBe(number?.fg.toString())
    } finally {
      setup.renderer.destroy()
    }
  }
})

test("an 80-column Edit block uses separate unified added and removed rows", async () => {
  const setup = await renderPart(
    completedPart({
      tool: "edit",
      input: { path: "src/main.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/main.ts",
      metadata: {
        diff: [
          "--- a/src/main.ts",
          "+++ b/src/main.ts",
          "@@ -1,1 +1,1 @@",
          "-const oldValue = 1",
          "+const newValue = 2",
        ].join("\n"),
        diagnostics: [
          { message: "first error", severity: 1, range: { start: { line: 0, character: 0 } } },
          { message: "second error", severity: 1, range: { start: { line: 4, character: 1 } } },
          { message: "third error", severity: 1, range: { start: { line: 8, character: 2 } } },
        ],
      },
    }),
    80,
  )

  try {
    await setup.waitForFrame((frame) => frame.includes("oldValue") && frame.includes("newValue"))
    await setup.waitForVisualIdle()
    const lines = frameText(setup.captureSpans()).split("\n")
    expect(cardTitle(lines.join("\n"))).toContain("edit [path=src/main.ts, edits=[1 item]]")

    const removedIndex = lines.findIndex((line) => line.includes("oldValue"))
    const addedIndex = lines.findIndex((line) => line.includes("newValue"))
    expect(removedIndex).toBeGreaterThanOrEqual(0)
    expect(addedIndex).toBeGreaterThanOrEqual(0)
    expect(removedIndex).not.toBe(addedIndex)
    expect(lines[removedIndex]).not.toContain("newValue")
    expect(lines[addedIndex]).not.toContain("oldValue")
    expect(lines.some((line) => line.includes("Error [1:1] first error"))).toBe(true)
    expect(lines.some((line) => line.includes("Error [5:2] second error"))).toBe(true)
    expect(lines.some((line) => line.includes("Error [9:3] third error"))).toBe(true)
  } finally {
    setup.renderer.destroy()
  }
})
test("wide Edit blocks place removed and added rows side by side", async () => {
  const setup = await renderPart(
    completedPart({
      tool: "edit",
      input: { path: "src/main.ts", edits: [{ type: "replace", line: 1 }] },
      output: "Edit applied successfully.",
      title: "Edit src/main.ts",
      metadata: {
        diff: [
          "--- a/src/main.ts",
          "+++ b/src/main.ts",
          "@@ -1,1 +1,1 @@",
          "-const oldValue = 1",
          "+const newValue = 2",
        ].join("\n"),
        diagnostics: [],
      },
    }),
    140,
  )

  try {
    await setup.waitForFrame((frame) => frame.includes("oldValue") && frame.includes("newValue"))
    const lines = frameText(setup.captureSpans()).split("\n")
    const removedIndex = lines.findIndex((line) => line.includes("oldValue"))
    const addedIndex = lines.findIndex((line) => line.includes("newValue"))
    expect(removedIndex).toBeGreaterThanOrEqual(0)
    expect(addedIndex).toBe(removedIndex)
    expect(lines[removedIndex]).toContain("oldValue")
    expect(lines[removedIndex]).toContain("newValue")
  } finally {
    setup.renderer.destroy()
  }
})

test("Write and Grep blocks render their bounded content and file groups at 80 and 140 columns", async () => {
  const write = completedPart({
    tool: "write",
    input: { path: "src/generated.ts", content: 'export const generated = "WRITE_VALUE"\n' },
    output: "Wrote src/generated.ts",
    title: "Write src/generated.ts",
    metadata: {
      display: {
        type: "file",
        path: "src/generated.ts",
        text: 'export const generated = "WRITE_VALUE"\n',
        lineStart: 1,
        truncated: false,
      },
      diagnostics: [],
    },
  })
  const grep = completedPart({
    tool: "grep",
    input: { pattern: "needle", path: "src" },
    output: "src/main.ts:3:const needle = 1\nsrc/config.json:2:\"needle\": true",
    title: "Grep needle",
    metadata: {
      display: {
        groups: [
          {
            path: "src/main.ts",
            rows: [
              { line: 3, text: "const needle = 1", isMatch: true },
              { line: 4, text: "return needle", isMatch: false },
            ],
          },
          {
            path: "src/config.json",
            rows: [{ line: 2, text: '"needle": true', isMatch: true }],
          },
        ],
      },
    },
  })

  for (const width of [80, 140]) {
    const writeSetup = await renderPart(write, width)
    try {
      await writeSetup.waitForFrame((frame) => frame.includes("WRITE_VALUE"))
      let syntaxReady = false
      for (let attempt = 0; attempt < 200 && !syntaxReady; attempt += 1) {
        await new Promise<void>((resolve) => setTimeout(resolve, 10))
        const pending = writeSetup.captureSpans()
        const keyword = bodySpanContaining(pending, "const")
        const string = bodySpanContaining(pending, "WRITE_VALUE")
        syntaxReady = keyword !== undefined && string !== undefined && keyword.fg.toString() !== string.fg.toString()
      }
      expect(syntaxReady).toBe(true)
      const writeFrame = writeSetup.captureSpans()
      const writeText = frameText(writeFrame)
      expect(cardTitle(writeText)).toContain("write [path=src/generated.ts")
      expect(writeText).toContain("WRITE_VALUE")
      expect(writeText).toMatch(/(?:^|\D)1(?:\D|$)/)
      const keyword = bodySpanContaining(writeFrame, "const")
      const string = bodySpanContaining(writeFrame, "WRITE_VALUE")
      expect(keyword).toBeDefined()
      expect(string).toBeDefined()
      expect(keyword?.fg.toString()).not.toBe(string?.fg.toString())
    } finally {
      writeSetup.renderer.destroy()
    }

    const grepSetup = await renderPart(grep, width)
    try {
      await grepSetup.waitForFrame((frame) => frame.includes("src/main.ts") && frame.includes("src/config.json"))
      await grepSetup.waitForVisualIdle()
      const grepText = frameText(grepSetup.captureSpans())
      expect(cardTitle(grepText)).toContain("grep [pattern=needle, path=src]")
      expect(grepText).toContain("src/main.ts")
      expect(grepText).toContain("src/config.json")
      expect(grepText).toContain("const needle = 1")
      expect(grepText).toContain('"needle": true')
      expect(grepText).toContain(">")
      expect(grepText).toContain("·")
    } finally {
      grepSetup.renderer.destroy()
    }
  }
})

test("Bash cards title-bar the command and keep output plain at 80 and 140 columns", async () => {
  const part = completedPart({
    tool: "bash",
    input: {
      command: 'printf "%s" "BASH_COMMAND_LITERAL"',
      cwd: "/work",
      env: { SECRET_ENV_VALUE: "must-not-render" },
    },
    output: "BASH_OUTPUT_ONLY",
    title: "Bash",
    metadata: { exit: 0, truncated: false },
  })

  for (const width of [80, 140]) {
    const setup = await renderPart(part, width)
    try {
      await setup.waitForFrame((frame) => frame.includes("BASH_OUTPUT_ONLY"))
      await setup.waitForVisualIdle()
      const captured = setup.captureSpans()
      const text = frameText(captured)
      expect(cardTitle(text)).toContain('$ printf "%s" "BASH_COMMAND_LITERAL"')
      expect(text).toContain("cwd /work")
      expect(text).toContain("BASH_OUTPUT_ONLY")
      expect(text).toContain("Completed · exit 0")
      expect(text).not.toContain("must-not-render")
      const command = spanContaining(captured, "BASH_COMMAND_LITERAL")
      const output = spanContaining(captured, "BASH_OUTPUT_ONLY")
      expect(command).toBeDefined()
      expect(output).toBeDefined()
      expect(command?.fg.toString()).not.toBe(output?.fg.toString())
    } finally {
      setup.renderer.destroy()
    }
  }
})

