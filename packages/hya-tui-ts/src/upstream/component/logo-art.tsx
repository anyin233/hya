import { RGBA } from "@opentui/core"
import { For } from "solid-js"
import { LOGO_ART } from "./logo-art.data"

// Each character encodes a 2x2 sub-pixel bitmap as a Unicode quadrant/half-block
// glyph; all ink shares the single art color and spaces stay transparent.
const INK = RGBA.fromInts(
  parseInt(LOGO_ART.color.slice(1, 3), 16),
  parseInt(LOGO_ART.color.slice(3, 5), 16),
  parseInt(LOGO_ART.color.slice(5, 7), 16),
)

export function LogoArt() {
  return (
    <box>
      <For each={LOGO_ART.rows}>
        {(row) => (
          <box flexDirection="row">
            <text fg={INK} selectable={false}>
              {row}
            </text>
          </box>
        )}
      </For>
    </box>
  )
}
