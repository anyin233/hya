/**
 * Assistant Markdown, rendered by OpenTUI's built-in `<markdown>` renderable
 * (`marked` block parsing plus tree-sitter highlighting in OpenTUI's parser
 * worker). This wrapper supplies the palette as a `SyntaxStyle` and draws
 * fenced code blocks as a panel-colored box with a language label.
 *
 * `streaming` keeps the trailing block unstable while deltas arrive, so an
 * unclosed fence or emphasis renders as plain text until it closes.
 * Highlighting covers the grammars bundled with @opentui/core (TypeScript,
 * JavaScript, Markdown, Zig); other languages render unhighlighted.
 */
import { BoxRenderable, SyntaxStyle, TextRenderable, type CodeRenderable, type MarkdownOptions, type MarkdownRenderable } from "@opentui/core"
import { createEffect } from "solid-js"
import { colors, syntaxStyles } from "../theme"

let shared: SyntaxStyle | undefined

/** The one SyntaxStyle every Markdown block shares (native object; created on first use). */
export function markdownSyntaxStyle(): SyntaxStyle {
  return (shared ??= SyntaxStyle.fromStyles(syntaxStyles))
}

/** Fenced code: the default code renderable inside a panel box, below a muted language label. */
const renderNode: NonNullable<MarkdownOptions["renderNode"]> = (token, context) => {
  if (token.type !== "code") return undefined
  const code = context.defaultRender() as CodeRenderable | null
  if (!code) return undefined
  // A custom block is rebuilt when its text changes, so draw the text at once
  // instead of waiting for the asynchronous highlight.
  code.drawUnstyledText = true
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
// Only code tokens are custom: the other blocks keep OpenTUI's coalesced
// Markdown rendering (blank lines between paragraphs, headings, and lists).
Object.assign(renderNode, { codeBlockOnly: true })

export function Markdown(props: { text: string; streaming?: boolean }) {
  let view: MarkdownRenderable | undefined
  // Content first, then the streaming flag, in one effect: OpenTUI's
  // non-streaming incremental parse reuses a previous token whose raw text is
  // a prefix of the new content, so an unclosed fence from the last delta
  // would stay open-ended. Outside streaming, a changed text is parsed afresh.
  createEffect(() => {
    const text = props.text
    const streaming = props.streaming ?? false
    if (!view) return
    if (view.content !== text) {
      if (!streaming) view._parseState = null
      view.content = text
    }
    view.streaming = streaming
  })
  return (
    // The style and the code-block renderer are in place before the effect
    // above sets the first content.
    <markdown
      ref={(element: MarkdownRenderable) => (view = element)}
      syntaxStyle={markdownSyntaxStyle()}
      fg={colors.fg}
      conceal
      renderNode={renderNode}
      width="100%"
    />
  )
}
