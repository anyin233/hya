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
  // Each row is one terminal cell tall so the parent reserves full art height
  // and the home tagline cannot stack into the descender row.
  return (
    <box flexDirection="column" flexShrink={0}>
      <For each={LOGO_ART.rows}>
        {(row) => (
          <box height={1} flexShrink={0} flexDirection="row">
            <text fg={INK} selectable={false}>
              {row}
            </text>
          </box>
        )}
      </For>
    </box>
  )
}
