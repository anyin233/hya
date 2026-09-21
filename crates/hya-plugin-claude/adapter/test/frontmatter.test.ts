import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs } from "./helpers"
import { agentMetaFrom, parseFrontmatter } from "../src/frontmatter"

afterEach(cleanupTempDirs)

describe("parseFrontmatter", () => {
  test("parses scalars, inline lists, and the body", () => {
    const text = "---\nname: Reviewer\ndescription: Reviews code\ntools: Read, Grep, Glob\nmodel: sonnet\n---\nBody text\n"
    const parsed = parseFrontmatter(text)
    expect(parsed.scalars["name"]).toBe("Reviewer")
    expect(parsed.scalars["description"]).toBe("Reviews code")
    expect(parsed.scalars["model"]).toBe("sonnet")
    expect(parsed.lists["tools"]).toEqual(["Read", "Grep", "Glob"])
    expect(parsed.body).toBe("Body text\n")
  })

  test("parses block lists", () => {
    const text = "---\ntools:\n  - Read\n  - Bash\n---\nBody\n"
    const parsed = parseFrontmatter(text)
    expect(parsed.lists["tools"]).toEqual(["Read", "Bash"])
  })

  test("handles files without frontmatter", () => {
    const parsed = parseFrontmatter("Just a body\n")
    expect(Object.keys(parsed.scalars)).toHaveLength(0)
    expect(parsed.body).toBe("Just a body\n")
  })
})

describe("agentMetaFrom", () => {
  test("extracts documented agent fields with defaults", () => {
    const meta = agentMetaFrom("---\nname: planner\ndescription: Plans work\n---\nDo planning.\n", "fallback")
    expect(meta.name).toBe("planner")
    expect(meta.description).toBe("Plans work")
    expect(meta.tools).toEqual([])
    expect(meta.model).toBe("inherit")
  })

  test("falls back to the file stem for the name", () => {
    const meta = agentMetaFrom("No frontmatter\n", "worker")
    expect(meta.name).toBe("worker")
    expect(meta.description).toBe("")
  })
})
