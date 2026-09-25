// Minimal OpenTUI app used to prove the browser harness renders and drives a
// real OpenTUI frontend: a bordered box, truecolor text, wide glyphs, the live
// terminal size, and an input that echoes on Enter.

import {
  BoxRenderable,
  CliRenderEvents,
  InputRenderable,
  InputRenderableEvents,
  TextRenderable,
  createCliRenderer,
} from "@opentui/core"

const renderer = await createCliRenderer({ exitOnCtrlC: true, targetFps: 30 })
const root = new BoxRenderable(renderer, {
  width: "100%",
  height: "100%",
  border: true,
  borderColor: "#405366",
  title: "probe",
  backgroundColor: "#11151b",
  flexDirection: "column",
})
const size = new TextRenderable(renderer, { content: "", fg: "#e8edf3" })
const accent = new TextRenderable(renderer, { content: "accent", fg: "#73c8e8" })
const wide = new TextRenderable(renderer, { content: "wide:你好|", fg: "#e8edf3" })
const echo = new TextRenderable(renderer, { content: "echo:", fg: "#e8edf3" })
const input = new InputRenderable(renderer, { width: 40, placeholder: "type here", textColor: "#e8edf3" })
for (const child of [size, accent, wide, echo, input]) root.add(child)
renderer.root.add(root)

const showSize = () => {
  size.content = `size ${renderer.width}x${renderer.height}`
}
showSize()
renderer.on(CliRenderEvents.RESIZE, showSize)
input.on(InputRenderableEvents.ENTER, (value: string) => {
  echo.content = `echo:${value}`
  input.value = ""
})
input.focus()
