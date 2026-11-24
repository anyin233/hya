import { expect, test } from "bun:test"
import type { SkillSummary } from "../src/client"
import type { KeyLike } from "../src/keys/bindings"
import { createSkillsController } from "../src/app/skills"
import { createAppStore } from "../src/state/store"
import {
  groupRule, hasSkillFile, initialSkillsView, orderedSkills, skillBodyLines, skillHeaderLine, skillLine, skillLocationText,
  skillsCountLine, skillsViewHint, skillsViewKey, skillsViewLines, type SkillsViewState,
} from "../src/state/skills"

const env = { HOME: "/home/u" }
const skills: SkillSummary[] = [
  { name: "reviewer", description: "Reviews code", source: "project", location: "/w/.hya/skills/reviewer/SKILL.md", content: "# Reviewer\n\nRead the diff." },
  { name: "pdf-tools", description: "Extract PDFs", source: "user", location: "/home/u/.codex/skills/pdf-tools/SKILL.md", content: "PDF" },
  { name: "release", description: "Cut a release", source: "project", location: "/w/.hya/skills/release/SKILL.md", content: "R" },
  { name: "secure-self-update", description: "Update hya", source: "builtin", location: "<built-in>", content: "S" },
]

const key = (name: string, sequence = ""): KeyLike => ({ name, sequence, ctrl: false, meta: false, shift: false } as KeyLike)
const char = (sequence: string): KeyLike => key(sequence, sequence)

test("the list groups skills under one divider per source directory, in catalog order", () => {
  const lines = skillsViewLines({ filter: "" }, skills, env)
  expect(lines.map((line) => line.kind === "divider" ? `-- ${line.title}` : line.skill.name)).toEqual([
    "-- project /w/.hya/skills", "reviewer", "release",
    "-- user ~/.codex/skills", "pdf-tools",
    "-- builtin", "secure-self-update",
  ])
  expect(orderedSkills({ filter: "" }, skills, env).map((skill) => skill.name)).toEqual(["reviewer", "release", "pdf-tools", "secure-self-update"])
})

test("rows show name, source, and description; the source column goes on a narrow screen", () => {
  expect(skillLine(skills[0]!, 80, skills, env)).toBe("reviewer           project  Reviews code")
  expect(skillHeaderLine(80, skills)).toBe("SKILL              SOURCE   DESCRIPTION")
  expect(skillLine(skills[0]!, 40, skills, env)).toBe("reviewer           Reviews code")
  expect(groupRule("builtin", 16)).toBe("── builtin ─────")
})

test("the filter matches the name, description, or group, and keeps the highlight on a shown row", () => {
  let view: SkillsViewState = initialSkillsView(skills)
  expect(view.skill).toBe("reviewer")
  for (const outcome of [skillsViewKey(view, char("/"), skills)]) if (outcome.type === "update") view = outcome.view
  for (const letter of "pdf") {
    const outcome = skillsViewKey(view, char(letter), skills)
    if (outcome.type === "update") view = outcome.view
  }
  expect(view.filter).toBe("pdf")
  expect(view.skill).toBe("pdf-tools")
  expect(initialSkillsView(skills, "builtin").skill).toBe("secure-self-update")
})

test("Up/Down walk the grouped order and wrap; Enter opens the details; Esc backs out, then closes", () => {
  let view = initialSkillsView(skills)
  const step = (pressed: KeyLike) => {
    const outcome = skillsViewKey(view, pressed, skills, { lines: 30, page: 10 })
    if (outcome.type === "update") view = outcome.view
    return outcome
  }
  step(key("up"))
  expect(view.skill).toBe("secure-self-update")
  step(key("down"))
  expect(view.skill).toBe("reviewer")
  step(key("down"))
  expect(view.skill).toBe("release")
  step(key("return"))
  expect(view.screen).toBe("detail")
  step(key("pagedown"))
  expect(view.scroll).toBe(9)
  step(key("end"))
  expect(view.scroll).toBe(20)
  step(key("down"))
  expect(view.scroll).toBe(20)
  step(key("home"))
  expect(view.scroll).toBe(0)
  expect(step(char("u"))).toEqual({ type: "use", skill: "release" })
  expect(step(char("y"))).toEqual({ type: "copy", skill: "release" })
  step(key("escape"))
  expect(view.screen).toBe("list")
  expect(step(key("escape"))).toEqual({ type: "close" })
  expect(skillsViewKey({ ...view, busy: { label: "Refreshing", startedAt: 0 } }, key("escape"), skills)).toEqual({ type: "cancelBusy" })
  expect(step(char("r"))).toEqual({ type: "refresh" })
})

test("an empty list answers Enter with a notice", () => {
  const outcome = skillsViewKey(initialSkillsView([]), key("return"), [])
  expect(outcome.type === "update" && outcome.view.notice?.text).toBe("No skill selected")
})

test("details text: the file, a built-in's origin, and the word-wrapped body", () => {
  expect(skillLocationText(skills[0]!)).toBe("/w/.hya/skills/reviewer/SKILL.md")
  expect(skillLocationText(skills[3]!)).toBe("<built-in> (hya/core-skills)")
  expect(hasSkillFile(skills[0]!)).toBe(true)
  expect(hasSkillFile(skills[3]!)).toBe(false)
  expect(skillBodyLines("one two three four\n\nxxxxxxxxxxxx", 9)).toEqual(["one two", "three", "four", "", "xxxxxxxxx", "xxx"])
  expect(skillsCountLine(skills)).toBe("4 skills")
  expect(skillsViewHint(initialSkillsView(skills))).toBe("↑↓ move · Enter details · u use · y copy path · r refresh · / filter · Esc close")
  expect(skillsViewHint({ ...initialSkillsView(skills), screen: "detail" })).toBe("↑↓ PgUp PgDn scroll · u use · y copy path · Esc back")
})

test("the controller opens on the skills read last, re-reads them, and fills the input on u", async () => {
  const store = createAppStore()
  store.setSkills(skills.slice(0, 1))
  const filled: string[] = []
  const copied: string[] = []
  let reads = 0
  const controller = createSkillsController({
    store,
    readSkills: async () => {
      reads++
      store.setSkills(skills)
      return skills
    },
    fillInput: (text) => { filled.push(text) },
    copyText: (text) => { copied.push(text); return true },
    rows: () => 24,
  })
  controller.open("pdf")
  expect(store.state.skillsView?.filter).toBe("pdf")
  await Bun.sleep(0)
  expect(reads).toBe(1)
  expect(store.state.skillsView?.skill).toBe("pdf-tools")
  controller.key(char("y"))
  expect(copied).toEqual(["/home/u/.codex/skills/pdf-tools/SKILL.md"])
  expect(store.state.skillsView?.notice?.text).toBe("Copied /home/u/.codex/skills/pdf-tools/SKILL.md")
  controller.key(char("r"))
  await Bun.sleep(0)
  expect(reads).toBe(2)
  expect(store.state.skillsView?.notice?.text).toBe("Refreshed · 4 skills")
  controller.key(char("u"))
  expect(filled).toEqual(["/skill:pdf-tools "])
  expect(store.state.skillsView).toBeUndefined()
})

test("y on a built-in skill says it has no file", () => {
  const store = createAppStore()
  store.setSkills(skills)
  const controller = createSkillsController({ store, readSkills: async () => skills, fillInput: () => undefined, copyText: () => true })
  controller.open("secure")
  controller.key(char("y"))
  expect(store.state.skillsView?.notice?.text).toBe("Built-in skill has no file")
})
