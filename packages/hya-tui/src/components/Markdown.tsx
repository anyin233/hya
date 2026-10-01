/**
 * Assistant Markdown, rendered by OpenTUI's built-in `<markdown>` renderable
 * (`marked` block parsing plus tree-sitter highlighting in OpenTUI's parser
 * worker). This wrapper supplies the palette as a `SyntaxStyle` and draws
 * fenced code blocks as a panel-colored box with a language label. A theme
 * switch (theme.ts `setTheme`) swaps the style and rebuilds the blocks.
 *
 * `streaming` keeps the trailing block unstable while deltas arrive, so an
 * unclosed fence or emphasis renders as plain text until it closes. Ordinary
 * Markdown blocks stay separate while streaming: OpenTUI's combined preview
 * otherwise exposes heading `###` markers before highlighting finishes.
 * Highlighting covers the grammars bundled with @opentui/core (TypeScript,
 * JavaScript, Markdown, Zig); other languages render unhighlighted.
 */
import { BoxRenderable, CodeRenderable, StyledText, SyntaxStyle, TextRenderable, bold, fg, type MarkdownOptions, type MarkdownRenderable } from "@opentui/core"
import { createEffect, on } from "solid-js"
import { colors, currentTheme, syntaxStylesFor, themeName } from "../theme"

const shared = new Map<string, SyntaxStyle>()

/**
 * The SyntaxStyle every Markdown block of the theme in effect shares (a
 * native object per theme, created on first use; reactive via `themeName`).
 */
export function markdownSyntaxStyle(): SyntaxStyle {
  const theme = currentTheme()
  let style = shared.get(theme.name)
  if (!style) shared.set(theme.name, (style = SyntaxStyle.fromStyles(syntaxStylesFor(theme))))
  return style
}

/** Fenced code: the default code renderable inside a panel box, below a muted language label. */
const renderNode: NonNullable<MarkdownOptions["renderNode"]> = (token, context) => {
  if (token.type !== "code") return undefined
  const code = context.defaultRender() as CodeRenderable | null
  if (!code) return undefined
  // A custom block is rebuilt when its text changes, so draw the text at once
  // instead of waiting for the asynchronous highlight.
  code.drawUnstyledText = true
  code.marginTop = 0
  code.marginBottom = 0
  const box = new BoxRenderable(code.ctx, {
    width: "100%",
    flexDirection: "column",
    flexShrink: 0,
    backgroundColor: colors.panel,
    paddingX: 1,
    marginBottom: 1,
  })
  const lang = typeof token.lang === "string" ? token.lang.trim().split(/\s+/)[0] : ""
  if (lang) box.add(new TextRenderable(code.ctx, { content: lang, fg: colors.muted, height: 1 }))
  box.add(code)
  return box
}
// Preserve the lexer tokens for headings. OpenTUI's code-block-only mode
// combines ordinary blocks into a synthetic paragraph, whose synchronous
// preview displays `###` until the asynchronous Markdown highlighter runs.
Object.assign(renderNode, { codeBlockOnly: false })

/** A marker-only heading at the end of a chunk has no text to show yet. */
function visibleStreamingText(text: string): string {
  const lineStart = text.lastIndexOf("\n") + 1
  if (!/^ {0,3}#{1,6}[ \t]*$/.test(text.slice(lineStart))) return text

  // A `###` line inside an open fenced code block is code, not a heading.
  let fence: { marker: string; length: number } | undefined
  for (const line of text.slice(0, lineStart).split("\n")) {
    const found = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line)
    if (!found) continue
    const marker = found[1]![0]!
    const length = found[1]!.length
    if (!fence) fence = { marker, length }
    else if (fence.marker === marker && length >= fence.length && !found[2]?.trim()) fence = undefined
  }
  return fence ? text : text.slice(0, lineStart)
}

/** Show heading text with its final accent while tree-sitter catches up. */
function previewStreamingHeadings(view: MarkdownRenderable, previewed: WeakMap<CodeRenderable, string>): void {
  for (const block of view._blockStates) {
    if (block.token.type !== "heading" || !(block.renderable instanceof CodeRenderable)) continue
    const heading = block.token.text
    if (!heading) continue
    const accent = colors.accent
    const signature = `${accent}\0${heading}`
    if (previewed.get(block.renderable) === signature) continue
    previewed.set(block.renderable, signature)
    const styled = new StyledText([bold(fg(accent)(heading))])
    block.renderable.updateStreamingPreview(block.renderable.content, styled)
  }
}

export function Markdown(props: { text: string; streaming?: boolean }) {
  let view: MarkdownRenderable | undefined
  const previewed = new WeakMap<CodeRenderable, string>()
  // Set streaming before content so the first chunk uses OpenTUI's provisional
  // parser. Outside streaming, a changed text is parsed afresh: reusing a
  // previous incomplete token could leave an unclosed fence open-ended.
  createEffect(() => {
    const text = props.text
    const streaming = props.streaming ?? false
    if (!view) return
    view.streaming = streaming
    const visible = streaming ? visibleStreamingText(text) : text
    if (view.content !== visible) {
      if (!streaming) view._parseState = null
      view.content = visible
    }
    if (streaming) previewStreamingHeadings(view, previewed)
  })
  // A theme switch: the new style, then every block rebuilt, so fenced code
  // boxes (drawn by `renderNode` with the palette of their time) repaint too.
  createEffect(on(themeName, () => {
    if (!view) return
    view.syntaxStyle = markdownSyntaxStyle()
    view.fg = colors.fg
    view.clearCache()
  }, { defer: true }))
  return (
    // The style and the code-block renderer are in place before the effect
    // above sets the first content.
    <markdown
      ref={(element: MarkdownRenderable) => (view = element)}
      syntaxStyle={markdownSyntaxStyle()}
      fg={colors.fg}
      conceal
      renderNode={renderNode}
      internalBlockMode="top-level"
      width="100%"
    />
  )
}
