import { afterEach, describe, expect, test } from "bun:test"

const ORIGINAL = process.env.HYA_STARTUP_TRACE

afterEach(() => {
  if (ORIGINAL === undefined) delete process.env.HYA_STARTUP_TRACE
  else process.env.HYA_STARTUP_TRACE = ORIGINAL
})

describe("startup-trace", () => {
  test("emits JSON mark lines when HYA_STARTUP_TRACE is set", async () => {
    process.env.HYA_STARTUP_TRACE = "1"
    // Fresh module so `enabled` re-reads env.
    const mod = await import(`../src/hya/startup-trace.ts?t=${Date.now()}`)
    const chunks: string[] = []
    const write = process.stderr.write.bind(process.stderr)
    process.stderr.write = ((chunk: string | Uint8Array, ...rest: unknown[]) => {
      chunks.push(typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8"))
      return write(chunk as never, ...(rest as never[]))
    }) as typeof process.stderr.write
    try {
      mod.startupMark("bun_entry", "test")
      mod.startupMark("bun_entry", "second") // once
    } finally {
      process.stderr.write = write
    }
    const lines = chunks.join("").split("\n").filter(Boolean)
    expect(lines.length).toBe(1)
    const payload = JSON.parse(lines[0]!) as {
      hya_startup: boolean
      mark: string
      wall_ms: number
      detail?: string
    }
    expect(payload.hya_startup).toBe(true)
    expect(payload.mark).toBe("bun_entry")
    expect(payload.detail).toBe("test")
    expect(typeof payload.wall_ms).toBe("number")
  })

  test("is a no-op when tracing is disabled", async () => {
    delete process.env.HYA_STARTUP_TRACE
    const mod = await import(`../src/hya/startup-trace.ts?disabled=${Date.now()}`)
    const chunks: string[] = []
    const write = process.stderr.write.bind(process.stderr)
    process.stderr.write = ((chunk: string | Uint8Array, ...rest: unknown[]) => {
      chunks.push(typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8"))
      return write(chunk as never, ...(rest as never[]))
    }) as typeof process.stderr.write
    try {
      mod.startupMark("sync_complete")
    } finally {
      process.stderr.write = write
    }
    expect(chunks.join("")).toBe("")
  })
})
