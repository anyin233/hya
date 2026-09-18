# 0.36.28

## Tool calls render as titled cards (TUI)

- Tool-calling output is now framed: every call renders as one rounded card with a
  blank row above and below its body, so calls are visually separated from
  assistant prose and reasoning instead of blending into the transcript.
- The border title carries the call itself — `$ <command>` for `bash`/`shell`,
  otherwise `tool [key=value, …]` — pretty-printed onto a single line and
  truncated with a trailing `…` against the measured card width. Paths in
  arguments use the session path formatter.
- Border and title colors state the call status: `warning` while pending or
  running, `error` on failure, `textMuted` on denial, and `borderSubtle` +
  `accent` once completed.
- `packages/hya-tui-ts/src/hya/tool-card.tsx` is the single card implementation.
  The session route's `InlineTool`/`BlockTool` split and the coding-tool
  `ToolPanel` are gone, as is `upstream/util/layout.ts`, whose sibling-margin
  plumbing the card's fixed spacing replaces.
- Card width is measured from `Renderable.onSizeChange`, which resolves inside
  the layout pass, so the fitted title paints in the same frame as the body. The
  previous `onLifecyclePass` measurement arrived a frame late and left every card
  untitled in a static transcript such as a replay.
- Launched subagent members that have no tool part yet stay unframed status
  rows; the frame belongs to real calls.
