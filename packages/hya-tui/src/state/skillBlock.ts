/**
 * A `/skill:<name> [request]` turn's user message (docs/skills.md "Invoke a
 * skill"): the backend sends the skill as a pi-style block, the request
 * after it,
 *
 * ```text
 * <skill name="reviewer" location="/w/.hya/skills/reviewer/SKILL.md">
 * References are relative to /w/.hya/skills/reviewer.
 *
 * <body>
 * </skill>
 *
 * fix auth
 * ```
 *
 * The transcript folds the block to one `▸ /skill:reviewer · 42 lines`
 * line with the request below it (components/MessageView.tsx). It reads the
 * durable message text, so every client (a reconnect, the WebUI, another
 * TUI) shows the same thing.
 */

export interface SkillBlock {
  name: string
  location: string
  /** The block's inner text: the reference line, if any, and the body. */
  content: string
  /** The request after the block; absent for a bare `/skill:<name>`. */
  request?: string
}

const pattern = /^<skill name="([^"]*)" location="([^"]*)">\n([\s\S]*)\n<\/skill>(?:\n\n([\s\S]+))?$/

function unescapeAttribute(value: string): string {
  return value.replace(/&(quot|lt|gt|amp);/g, (_, entity: string) => ({ quot: "\"", lt: "<", gt: ">", amp: "&" })[entity]!)
}

/**
 * The skill block that is the whole of `text`, or `undefined`. The body may
 * itself contain `\n</skill>`: the block ends at the last closing tag that
 * is followed by nothing or by a blank line and the request.
 */
export function parseSkillBlock(text: string): SkillBlock | undefined {
  if (!text.startsWith("<skill name=\"")) return undefined
  const match = pattern.exec(text)
  if (!match) return undefined
  const request = match[4]?.trim()
  return {
    name: unescapeAttribute(match[1]!),
    location: unescapeAttribute(match[2]!),
    content: match[3]!,
    ...(request ? { request } : {}),
  }
}

/** `▸ /skill:reviewer · 42 lines` (`▾` when unfolded). */
export function skillBlockLabel(block: SkillBlock, unfolded: boolean): string {
  const lines = block.content.split("\n").length
  return `${unfolded ? "▾" : "▸"} /skill:${block.name} · ${lines} line${lines === 1 ? "" : "s"}`
}
