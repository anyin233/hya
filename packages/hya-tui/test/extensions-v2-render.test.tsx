import { afterEach, expect, test } from "bun:test"
import { testRender } from "@opentui/solid"
import { RenderTree } from "../src/extensions/renderTree"
import { nodeText, validateRenderNode } from "../src/extensions/wire"

let setup: Awaited<ReturnType<typeof testRender>> | undefined
afterEach(() => { setup?.renderer.destroy(); setup = undefined })

test("RenderTree delegates host-virtualized { type: 'transcript', sessionId } nodes directly to TreeHost.transcript", async () => {
  const seen: unknown[] = []
  const validated = validateRenderNode(
    {
      kind: "column",
      children: [
        { kind: "text", text: "Subagent Header" },
        { type: "transcript", sessionId: "hysec_child_42", follow: true, compact: true },
      ],
    },
    { slot: "forbid", actions: false, transcriptEmbed: true },
  )
  if (typeof validated === "string") throw new Error(validated)
  expect(nodeText(validated)).toBe("Subagent Header\n[transcript:hysec_child_42]")

  setup = await testRender(() => (
    <RenderTree
      node={validated}
      host={{
        transcript: (node) => {
          seen.push(node)
          return <text>{`VIRTUALIZED:${node.sessionId}:follow=${String(node.follow)}`}</text>
        },
      }}
    />
  ), { width: 60, height: 8 })
  await setup.renderOnce()
  const frame = setup.captureCharFrame()
  expect(frame).toContain("Subagent Header")
  expect(frame).toContain("VIRTUALIZED:hysec_child_42:follow=true")
  expect(seen).toEqual([{ type: "transcript", sessionId: "hysec_child_42", follow: true, compact: true }])
})

test("RenderTree renders a safe placeholder when no TreeHost.transcript delegate is supplied", async () => {
  const validated = validateRenderNode(
    { type: "transcript", sessionId: "hysec_child_7", follow: false, scrollOffset: 12 },
    { slot: "forbid", actions: false, transcriptEmbed: true },
  )
  if (typeof validated === "string") throw new Error(validated)
  setup = await testRender(() => <RenderTree node={validated} />, { width: 60, height: 4 })
  await setup.renderOnce()
  expect(setup.captureCharFrame()).toContain("[transcript: hysec_child_7 · scroll:12]")
})
