import { describe, expect, test } from "bun:test"

import {
  parseContextStatus,
  presentContextStatus,
  type ContextStatus,
} from "../src/hya/context-status"

describe("parseContextStatus", () => {
  test("parses a well-formed context block", () => {
    const status = parseContextStatus({
      tokens: 12_345,
      source: "estimate",
      mode: "auto",
      threshold: 150_000,
    })
    expect(status).toEqual({
      tokens: 12_345,
      source: "estimate",
      mode: "auto",
      threshold: 150_000,
    })
  })

  test("returns undefined for absent or malformed values", () => {
    expect(parseContextStatus(undefined)).toBeUndefined()
    expect(parseContextStatus(null)).toBeUndefined()
    expect(parseContextStatus("nope")).toBeUndefined()
    expect(parseContextStatus({})).toBeUndefined()
    expect(parseContextStatus({ tokens: 1, source: "nope", mode: "auto", threshold: 2 })).toBeUndefined()
    expect(parseContextStatus({ tokens: 1, source: "estimate", mode: "auto" })).toBeUndefined()
    expect(parseContextStatus({ tokens: -1, source: "estimate", mode: "auto", threshold: 2 })).toBeUndefined()
  })
})

describe("presentContextStatus", () => {
  const estimated: ContextStatus = {
    tokens: 30_000,
    source: "estimate",
    mode: "auto",
    threshold: 150_000,
  }

  test("reports occupancy against the resolved threshold", () => {
    const view = presentContextStatus(estimated)
    expect(view.tokens).toBe("30,000")
    expect(view.limit).toBe("150,000")
    expect(view.percent).toBe(20)
  })

  test("flags estimated figures so provider-unreliable fallback is visible", () => {
    expect(presentContextStatus(estimated).badge).toBe("estimated")
    const provider: ContextStatus = {
      tokens: 30_000,
      source: "provider",
      mode: "auto",
      threshold: 150_000,
    }
    expect(presentContextStatus(provider).badge).toBeNull()
  })

  test("clamps percent to 100 when the count passed the threshold", () => {
    const over: ContextStatus = { ...estimated, tokens: 180_000 }
    expect(presentContextStatus(over).percent).toBe(100)
  })
})
