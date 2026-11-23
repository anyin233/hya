import { afterEach, describe, expect, test } from "bun:test"
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { decodeServerFrame, encodeClientFrame, type ServerFrame } from "../src/frames"
import { startHost, type Host } from "../src/host"
import { maxUploadBytes, maxUploadsPerConnection, pastedPathsText, sanitizeUploadName, uploadRoot, UploadStore } from "../src/uploads"

let host: Host | undefined
afterEach(async () => {
  await host?.stop()
  host = undefined
})

describe("uploads (ADR-0037)", () => {
  test("names are reduced to one safe segment", () => {
    expect(sanitizeUploadName("shot 1.png")).toBe("shot 1.png")
    expect(sanitizeUploadName("../../etc/passwd")).toBe("passwd")
    expect(sanitizeUploadName("C:\\\\Users\\\\me\\\\a b.png")).toBe("a b.png")
    expect(sanitizeUploadName(".hidden")).toBe("hidden")
    expect(sanitizeUploadName("it's $(rm).png")).toBe("it_s __rm_.png")
    expect(sanitizeUploadName("")).toBe("upload")
    expect(sanitizeUploadName("x".repeat(300) + ".png").length).toBe(120)
    expect(pastedPathsText(["/tmp/a b.png", "/tmp/c'd.png"])).toBe(`'/tmp/a b.png' '/tmp/c'\\''d.png'`)
  })

  test("a store keeps files private to its connection, refuses oversized and extra files, and clears", () => {
    const root = mkdtempSync(join(tmpdir(), "uploads-"))
    try {
      const store = new UploadStore(root, 7)
      const first = store.store({ id: "a", name: "shot.png", mime: "image/png", data: new Uint8Array([1, 2, 3]) })
      expect(first).toEqual({ uploaded: { id: "a", path: join(root, "7", "shot.png") } })
      expect(statSync(join(root, "7")).mode & 0o777).toBe(0o700)
      expect([...readFileSync(join(root, "7", "shot.png"))]).toEqual([1, 2, 3])
      expect(store.store({ id: "b", name: "shot.png", mime: "", data: new Uint8Array([4]) })).toEqual({ uploaded: { id: "b", path: join(root, "7", "2-shot.png") } })
      expect(store.store({ id: "c", name: "big.bin", mime: "", data: new Uint8Array(maxUploadBytes + 1) })).toEqual({ uploadFailed: { id: "c", reason: "larger than 20 MiB" } })
      expect(store.store({ id: "d", name: "empty", mime: "", data: new Uint8Array() })).toEqual({ uploadFailed: { id: "d", reason: "empty file" } })
      for (let n = 2; n < maxUploadsPerConnection; n++) store.store({ id: `x${n}`, name: "f", mime: "", data: new Uint8Array([n]) })
      expect(store.store({ id: "over", name: "f", mime: "", data: new Uint8Array([1]) })).toEqual({ uploadFailed: { id: "over", reason: `at most ${maxUploadsPerConnection} files per tab` } })
      store.clear()
      expect(existsSync(join(root, "7"))).toBe(false)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  test("the host stores an upload frame, answers with the path, and removes it when the tab closes", async () => {
    host = startHost({ command: ["sh", "-c", "sleep 30"], port: 0 })
    const ws = new WebSocket(`${host.url.replace("http", "ws")}pty?cols=80&rows=24`)
    const answers: ServerFrame[] = []
    ws.addEventListener("message", (event) => {
      const frame = decodeServerFrame(String(event.data))
      if (frame && ("uploaded" in frame || "uploadFailed" in frame)) answers.push(frame)
    })
    await new Promise((resolve) => ws.addEventListener("open", resolve))
    ws.send(encodeClientFrame({ upload: { id: "u1", name: "drop me.txt", mime: "text/plain", data: new TextEncoder().encode("hi\n") } }))
    const deadline = Date.now() + 5000
    while (!answers.length && Date.now() < deadline) await Bun.sleep(20)
    const answer = answers[0]
    if (!answer || !("uploaded" in answer)) throw new Error(`no upload answer: ${JSON.stringify(answers)}`)
    expect(answer.uploaded.id).toBe("u1")
    expect(answer.uploaded.path.startsWith(uploadRoot())).toBe(true)
    expect(readFileSync(answer.uploaded.path, "utf8")).toBe("hi\n")
    ws.close()
    const gone = Date.now() + 5000
    while (existsSync(answer.uploaded.path) && Date.now() < gone) await Bun.sleep(20)
    expect(existsSync(answer.uploaded.path)).toBe(false)
  })
})
