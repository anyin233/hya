// Pure-logic unit tests for hya-extra/model-fallback. `fallback.ts` guards its
// stdio loop with `if (import.meta.main)`, so importing it here never spawns
// the JSON-RPC loop — only `parseConfig` / `decide` run.
import { describe, expect, test } from "bun:test"

import { DEFAULT_CONFIG, type FallbackConfig, type FallbackParams, decide, parseConfig } from "./fallback"

function params(overrides: Partial<FallbackParams> = {}): FallbackParams {
  return {
    session: "session-1",
    root_session: "session-1",
    agent: "build",
    message: "message-1",
    model: "anthropic/claude-opus-5-5",
    error: { class: "retryable", message: "http status 529: overloaded" },
    attempt: 1,
    tried: ["anthropic/claude-opus-5-5"],
    ...overrides,
  }
}

describe("decide", () => {
  test("walks the ORIGINAL failing model's chain, not the model that just failed", () => {
    const config: FallbackConfig = {
      chains: {
        "acme/a": ["acme/b", "acme/c"],
        "acme/b": ["acme/z"],
      },
      default: [],
      on: ["retryable"],
      max_attempts: 3,
    }
    // Round: a -> b (b failed too); tried[0] is still "acme/a", so the next
    // candidate must come from a's chain ("acme/c"), never b's ("acme/z").
    const outcome = decide(
      params({ model: "acme/b", tried: ["acme/a", "acme/b"] }),
      config,
    )
    expect(outcome).toEqual({ outcome: "retry", model: "acme/c" })
  })

  test("skips models already tried in the chain", () => {
    const config: FallbackConfig = {
      chains: { "acme/a": ["acme/b", "acme/c"] },
      default: [],
      on: ["retryable"],
      max_attempts: 3,
    }
    const outcome = decide(
      params({ tried: ["acme/a", "acme/b"], model: "acme/b" }),
      config,
    )
    expect(outcome).toEqual({ outcome: "retry", model: "acme/c" })
  })

  test("filters by configured error classes", () => {
    const config: FallbackConfig = {
      chains: { "acme/a": ["acme/b"] },
      default: [],
      on: ["auth"],
      max_attempts: 3,
    }
    const outcome = decide(params({ error: { class: "retryable", message: "x" } }), config)
    expect(outcome).toEqual({ outcome: "give_up" })
  });

  test("`on: [any]` matches every error class", () => {
    const config: FallbackConfig = {
      chains: { "acme/a": ["acme/b"] },
      default: [],
      on: ["any"],
      max_attempts: 3,
    }
    for (const errorClass of ["retryable", "unknown_model", "auth", "invalid_request", "other"]) {
      const outcome = decide(
        params({ model: "acme/a", tried: ["acme/a"], error: { class: errorClass, message: "x" } }),
        config,
      )
      expect(outcome).toEqual({ outcome: "retry", model: "acme/b" })
    }
  })

  test("falls back to the default chain for a model with no chains entry", () => {
    const config: FallbackConfig = {
      chains: { "acme/other": ["acme/never"] },
      default: ["acme/fallback"],
      on: ["retryable"],
      max_attempts: 3,
    }
    const outcome = decide(params({ model: "acme/unlisted", tried: ["acme/unlisted"] }), config)
    expect(outcome).toEqual({ outcome: "retry", model: "acme/fallback" })
  })

  test("gives up once attempt exceeds max_attempts", () => {
    const config: FallbackConfig = {
      chains: { "acme/a": ["acme/b", "acme/c"] },
      default: [],
      on: ["retryable"],
      max_attempts: 2,
    }
    const outcome = decide(params({ attempt: 3, tried: ["acme/a", "acme/b"] }), config)
    expect(outcome).toEqual({ outcome: "give_up" })
  })

  test("gives up when the chain is exhausted", () => {
    const config: FallbackConfig = {
      chains: { "acme/a": ["acme/b"] },
      default: [],
      on: ["retryable"],
      max_attempts: 8,
    }
    const outcome = decide(params({ tried: ["acme/a", "acme/b"] }), config)
    expect(outcome).toEqual({ outcome: "give_up" })
  })

  test("gives up when the model has no chain and no default", () => {
    const outcome = decide(params({ model: "acme/unlisted", tried: ["acme/unlisted"] }), DEFAULT_CONFIG)
    expect(outcome).toEqual({ outcome: "give_up" })
  })

  test("never throws: a malformed params shape reads as give-up", () => {
    const broken = { ...params(), error: undefined } as unknown as FallbackParams
    expect(() => decide(broken, DEFAULT_CONFIG)).not.toThrow()
    expect(decide(broken, DEFAULT_CONFIG)).toEqual({ outcome: "give_up" })
  })
})

describe("parseConfig", () => {
  test("missing config text uses DEFAULT_CONFIG", () => {
    expect(parseConfig(undefined)).toEqual(DEFAULT_CONFIG)
    expect(parseConfig("")).toEqual(DEFAULT_CONFIG)
    expect(parseConfig("   \n")).toEqual(DEFAULT_CONFIG)
  })

  test("invalid YAML falls back to DEFAULT_CONFIG instead of throwing", () => {
    expect(() => parseConfig("chains: [unterminated")).not.toThrow()
    expect(parseConfig("chains: [unterminated")).toEqual(DEFAULT_CONFIG)
  })

  test("a YAML document that is not a mapping falls back to DEFAULT_CONFIG", () => {
    expect(parseConfig("- just\n- a\n- list\n")).toEqual(DEFAULT_CONFIG)
  })

  test("parses a full config.yml", () => {
    const text = `
chains:
  anthropic/claude-opus-5-5: [anthropic/claude-sonnet-5, openai/gpt-5.5]
default: [openai/gpt-5.5]
on: [retryable, unknown_model]
max_attempts: 5
`
    expect(parseConfig(text)).toEqual({
      chains: { "anthropic/claude-opus-5-5": ["anthropic/claude-sonnet-5", "openai/gpt-5.5"] },
      default: ["openai/gpt-5.5"],
      on: ["retryable", "unknown_model"],
      max_attempts: 5,
    })
  })

  test("an invalid chains entry is dropped, not fatal", () => {
    const text = `
chains:
  acme/a: [acme/b]
  acme/broken: not-a-list
`
    const config = parseConfig(text)
    expect(config.chains).toEqual({ "acme/a": ["acme/b"] })
  })

  test("max_attempts above the engine cap of 8 is clamped, not rejected", () => {
    const config = parseConfig("max_attempts: 20\n")
    expect(config.max_attempts).toBe(8)
  })

  test("an unknown error class in `on` falls back to the default `on` list", () => {
    const config = parseConfig("on: [not_a_real_class]\n")
    expect(config.on).toEqual(DEFAULT_CONFIG.on)
  })
})
