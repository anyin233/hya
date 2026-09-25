import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import catalog from "../src/operations.json"
import { apiOperationNames, operations } from "../src/api"

type Paths = Record<string, Record<string, { operationId?: string; "x-server-streaming"?: boolean }>>

/** The repository's OpenAPI document; the package itself never imports it (it ships alone under lib/hya/tui). */
const openapi = JSON.parse(readFileSync(fileURLToPath(new URL("../../../docs/protocol/openapi.json", import.meta.url)), "utf8")) as { paths: Paths }

test("the packaged operation catalog is the one in docs/protocol/openapi.json (regenerate with cargo run -p xtask -- gen-api)", () => {
  const expected = Object.entries(openapi.paths).flatMap(([path, methods]) =>
    Object.entries(methods).map(([method, detail]) => ({
      method: method.toUpperCase(), path, operationId: detail.operationId ?? "", streaming: detail["x-server-streaming"] ?? false,
    })),
  )
  const key = (op: { method: string; path: string }) => `${op.path} ${op.method}`
  expect([...catalog].sort((a, b) => key(a).localeCompare(key(b)))).toEqual(expected.sort((a, b) => key(a).localeCompare(key(b))))
})

test("/api lists every operation with its streaming marker and completes METHOD /path names", () => {
  expect(apiOperationNames).toContain("GET /v1/sessions")
  expect(operations()).toMatch(/^GET\s+\/v1\/sessions\s+Session\.ListSessions$/m)
  expect(operations()).toContain(" [stream]")
  expect(apiOperationNames.length).toBe(operations().split("\n").length)
})

test("no source file imports from outside the package (it ships alone under lib/hya/tui)", () => {
  const root = fileURLToPath(new URL("..", import.meta.url))
  const escaping: string[] = []
  for (const file of new Bun.Glob("src/**/*.{ts,tsx,json}").scanSync({ cwd: root })) {
    const text = readFileSync(join(root, file), "utf8")
    for (const match of text.matchAll(/(?:from|import)\s*\(?\s*["'](\.{1,2}\/[^"']*)["']/g)) {
      const target = resolve(dirname(join(root, file)), match[1]!)
      if (!target.startsWith(`${resolve(root)}/`)) escaping.push(`${file}: ${match[1]}`)
    }
  }
  expect(escaping).toEqual([])
})
