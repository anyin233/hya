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

  test("uploads and their answers round-trip (ADR-0037)", () => {
    const data = new Uint8Array([0x89, 0x50, 0x4e, 0x47])
    const text = encodeClientFrame({ upload: { id: "u1", name: "shot 1.png", mime: "image/png", data } })
    expect(JSON.parse(text)).toEqual({ upload: { id: "u1", name: "shot 1.png", mime: "image/png", data: "iVBORw==" } })
    expect(decodeClientFrame(text)).toEqual({ upload: { id: "u1", name: "shot 1.png", mime: "image/png", data } })
    // protojson omits empty strings.
    expect(decodeClientFrame(JSON.stringify({ upload: { id: "u2", data: "" } }))).toEqual({ upload: { id: "u2", name: "", mime: "", data: new Uint8Array() } })
    expect(decodeServerFrame(encodeServerFrame({ uploaded: { id: "u1", path: "/tmp/x/shot 1.png" } }))).toEqual({ uploaded: { id: "u1", path: "/tmp/x/shot 1.png" } })
    expect(JSON.parse(encodeServerFrame({ uploadFailed: { id: "u1", reason: "too large" } }))).toEqual({ uploadFailed: { id: "u1", reason: "too large" } })
    expect(decodeServerFrame(JSON.stringify({ uploadFailed: { id: "u1", reason: "uploads are not supported by hya serve PTY sessions" } }))).toEqual({ uploadFailed: { id: "u1", reason: "uploads are not supported by hya serve PTY sessions" } })
    expect(decodeClientFrame(JSON.stringify({ upload: { id: 3, data: "" } }))).toBeNull()
  })

  test("malformed or unknown frames decode to null", () => {
    expect(decodeClientFrame("not json")).toBeNull()
    expect(decodeClientFrame(JSON.stringify({ attach: { id: "x" } }))).toBeNull()
    expect(decodeClientFrame(JSON.stringify({ resize: { cols: 0, rows: 10 } }))).toBeNull()
    expect(decodeServerFrame("[]")).toBeNull()
  })
})
