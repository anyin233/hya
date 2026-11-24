import { expect, test } from "bun:test"
import { parseSkillBlock, skillBlockLabel } from "../src/state/skillBlock"

const block = "<skill name=\"reviewer\" location=\"/w/.hya/skills/reviewer/SKILL.md\">\nReferences are relative to /w/.hya/skills/reviewer.\n\n# Reviewer\nRead the diff.\n</skill>"

test("a skill block with its request parses into the name, location, content, and request", () => {
  expect(parseSkillBlock(`${block}\n\nfix auth\nplease`)).toEqual({
    name: "reviewer",
    location: "/w/.hya/skills/reviewer/SKILL.md",
    content: "References are relative to /w/.hya/skills/reviewer.\n\n# Reviewer\nRead the diff.",
    request: "fix auth\nplease",
  })
  expect(parseSkillBlock(block)?.request).toBeUndefined()
})

test("attribute entities are unescaped and a body may hold its own closing tag", () => {
  const parsed = parseSkillBlock("<skill name=\"b\" location=\"embedded:x/&quot;q&quot;&amp;&lt;&gt;\">\nsay </skill>\n</skill> here\n</skill>\n\nreq")
  expect(parsed?.location).toBe("embedded:x/\"q\"&<>")
  expect(parsed?.content).toBe("say </skill>\n</skill> here")
  expect(parsed?.request).toBe("req")
})

test("text that is not one whole skill block does not parse", () => {
  expect(parseSkillBlock("hello")).toBeUndefined()
  expect(parseSkillBlock(`before\n${block}`)).toBeUndefined()
  expect(parseSkillBlock(`${block}\ntrailing on the next line`)).toBeUndefined()
})

test("the folded label names the skill command and its line count", () => {
  const parsed = parseSkillBlock(block)!
  expect(skillBlockLabel(parsed, false)).toBe("▸ /skill:reviewer · 4 lines")
  expect(skillBlockLabel(parsed, true)).toBe("▾ /skill:reviewer · 4 lines")
})
