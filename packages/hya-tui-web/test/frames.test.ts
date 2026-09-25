import { describe, expect, test } from "bun:test"
import { decodeClientFrame, decodeServerFrame, encodeClientFrame, encodeServerFrame } from "../src/frames"

describe("hya.v1 PTY frames", () => {
  test("output bytes travel as protojson base64", () => {
    const bytes = new TextEncoder().encode("\x1b[1mhé\x1b[0m")
    const text = encodeServerFrame({ output: bytes })
    expect(JSON.parse(text)).toEqual({ output: Buffer.from(bytes).toString("base64") })
    expect(decodeServerFrame(text)).toEqual({ output: bytes })
  })

  test("exit, pong, resize, input and ping round-trip", () => {
    expect(decodeServerFrame(encodeServerFrame({ exit: 3 }))).toEqual({ exit: 3 })
    expect(decodeServerFrame(encodeServerFrame({ pong: true }))).toEqual({ pong: true })
    expect(decodeClientFrame(encodeClientFrame({ resize: { cols: 120, rows: 40 } }))).toEqual({ resize: { cols: 120, rows: 40 } })
    const input = new TextEncoder().encode("q\r")
    expect(decodeClientFrame(encodeClientFrame({ input }))).toEqual({ input })
    expect(decodeClientFrame(encodeClientFrame({ ping: true }))).toEqual({ ping: true })
  })

  test("malformed or unknown frames decode to null", () => {
    expect(decodeClientFrame("not json")).toBeNull()
    expect(decodeClientFrame(JSON.stringify({ attach: { id: "x" } }))).toBeNull()
    expect(decodeClientFrame(JSON.stringify({ resize: { cols: 0, rows: 10 } }))).toBeNull()
    expect(decodeServerFrame("[]")).toBeNull()
  })
})
