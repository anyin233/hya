import { afterEach, expect, test } from "bun:test"
import type { RGBA } from "@opentui/core"
import { testRender } from "@opentui/solid"
import { batch, createSignal } from "solid-js"
import { Markdown } from "../src/components/Markdown"
import { colors, defaultThemeName, setTheme, syntaxColors, themes } from "../src/theme"

type Setup = Awaited<ReturnType<typeof testRender>>
let setup: Setup | undefined

afterEach(() => {
  setTheme(defaultThemeName)
  setup?.renderer.destroy()
  setup = undefined
})

function hex(color: RGBA | undefined): string {
  if (!color) return "none"
  const [r, g, b] = [color.r, color.g, color.b].map((value) => Math.round(value * 255).toString(16).padStart(2, "0"))
  return `#${r}${g}${b}`
}

async function render(text: () => string, streaming: () => boolean = () => false, width = 60): Promise<Setup> {
  setup = await testRender(() => (
    <box width={width} flexDirection="column">
      <Markdown text={text()} streaming={streaming()} />
    </box>
  ), { width, height: 30 })
  return setup
}

/** Render until `predicate` holds for the frame (tree-sitter highlighting is asynchronous). */
async function until(predicate: () => boolean, what: string): Promise<void> {
  // Up to ~5 s: the first highlight loads the tree-sitter worker, slow on a cold or busy machine.
  for (let pass = 0; pass < 250; pass++) {
    await setup!.renderOnce()
    if (predicate()) return
    await Bun.sleep(20)
  }
  throw new Error(`never rendered: ${what}\n${setup!.captureCharFrame()}`)
}

function span(text: string) {
  for (const line of setup!.captureSpans().lines) {
    for (const item of line.spans) if (item.text.includes(text)) return item
  }
  return undefined
}

function frame(): string[] {
  return setup!.captureCharFrame().split("\n").map((line) => line.trimEnd())
}

test("renders headings, emphasis, inline code, and links with the palette", async () => {
  await render(() => "# Release notes\n\nSome **bold**, *soft*, and `npm test` via [docs](https://example.test/docs).\n\n> quoted line", undefined, 90)
  await until(() => hex(span("Release notes")?.fg) === colors.accent, "accent heading")
  const lines = frame()
  expect(lines).toContain("Release notes")
  // Blocks keep their blank line between them.
  expect(lines[1]).toBe("")
  expect(lines.some((line) => line.includes("#"))).toBe(false)
  expect(span("bold")!.attributes & 1).toBe(1)
  expect(span("soft")!.attributes & 4).toBe(4)
  expect(hex(span("npm test")?.fg)).toBe(syntaxColors.inlineCode)
  // The link keeps its URL visible after the label.
  expect(lines.some((line) => line.includes("docs (https://example.test/docs)"))).toBe(true)
  expect(lines.some((line) => /│ quoted line/.test(line))).toBe(true)
})

test("renders nested lists with indentation", async () => {
  await render(() => "- one\n  - nested\n    - deeper\n- two\n\n1. first\n2. second")
  await until(() => frame().includes("  - nested"), "nested list")
  const lines = frame()
  expect(lines).toContain("- one")
  expect(lines).toContain("    - deeper")
  expect(lines).toContain("1. first")
})

test("fenced code blocks sit on the panel color with a language label and highlighted tokens", async () => {
  await render(() => "Before\n\n```ts\nconst answer = \"forty-two\"\n```\n\nAfter")
  await until(() => hex(span("const")?.fg) === syntaxColors.keyword, "highlighted keyword")
  expect(hex(span("\"forty-two\"")?.fg)).toBe(syntaxColors.string)
  expect(hex(span("const")?.bg)).toBe(colors.panel)
  const lines = frame()
  const label = lines.findIndex((line) => line.trim() === "ts")
  expect(label).toBeGreaterThan(0)
  expect(lines[label + 1]?.trim()).toBe("const answer = \"forty-two\"")
  expect(lines.some((line) => line.includes("```"))).toBe(false)
  expect(lines).toContain("After")
})

test("a code block of an unknown language still renders its text", async () => {
  await render(() => "```python\nprint('hi')\n```")
  await until(() => frame().some((line) => line.includes("print('hi')")), "python code")
  expect(hex(span("print('hi')")?.bg)).toBe(colors.panel)
})

test("partial markdown while streaming: an unclosed fence and unclosed emphasis still show their text", async () => {
  const [text, setText] = createSignal("Intro **bol")
  const [streaming, setStreaming] = createSignal(true)
  await render(text, streaming)
  await until(() => frame().some((line) => line.includes("Intro") && line.includes("bol")), "partial emphasis")
  setText("Intro **bold** done\n\n```ts\nconst x = 1\nlet y")
  await until(() => frame().some((line) => line.includes("let y")), "unclosed fence content")
  expect(frame().some((line) => line.includes("```"))).toBe(false)
  setText("Intro **bold** done\n\n```ts\nconst x = 1\nlet y = 2\n```\n\nTail")
  setStreaming(false)
  await until(() => frame().includes("Tail") && frame().some((line) => line.includes("let y = 2")), "closed fence")
  expect(span("bold")!.attributes & 1).toBe(1)
})

test("a reply whose stream ends with the last delta re-parses: an unclosed fence from a delta never splits the block", async () => {
  const reply = "Intro\n\n- item\n\n```ts\nconst answer = \"forty-two\"\n```\n\nDone."
  for (const order of ["same update", "streaming ends first"] as const) {
    const [text, setText] = createSignal("")
    const [streaming, setStreaming] = createSignal(true)
    await render(text, streaming)
    // The last delta stops inside the fenced code (`…forty-tw`), like a 16-character chunking does.
    for (let at = 16; at < reply.length; at += 16) {
      setText(reply.slice(0, at))
      await setup!.renderOnce()
    }
    if (order === "same update") batch(() => { setText(reply); setStreaming(false) })
    else {
      setStreaming(false)
      await setup!.renderOnce()
      setText(reply)
    }
    await until(() => frame().includes("Done.") && frame().some((line) => line.includes("const answer = \"forty-two\"")), `whole code block (${order})`)
    expect(frame().some((line) => line.trim() === "o\"")).toBe(false)
    expect(hex(span("Done.")?.bg)).not.toBe(colors.panel)
    setup!.renderer.destroy()
    setup = undefined
  }
})

test("switching the theme re-renders rendered Markdown: headings, highlighted code, and the code panel", async () => {
  await render(() => "# Title\n\n```ts\nconst answer = 1\n```\n\nAfter")
  await until(() => hex(span("const")?.fg) === themes.hya.syntaxColors.keyword, "hya keyword")
  await until(() => hex(span("Title")?.fg) === themes.hya.colors.accent, "hya heading")
  setTheme("light")
  await until(() => hex(span("const")?.fg) === themes.light.syntaxColors.keyword, "light keyword")
  await until(() => hex(span("Title")?.fg) === themes.light.colors.accent, "light heading")
  await until(() => hex(span("After")?.fg) === themes.light.colors.fg, "light paragraph")
  expect(hex(span("const")?.bg)).toBe(themes.light.colors.panel)
})
