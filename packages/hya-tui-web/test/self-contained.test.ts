import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

test("no host or page file imports from outside the package (it ships alone under lib/hya/tui-web)", () => {
  const root = fileURLToPath(new URL("..", import.meta.url))
  const escaping: string[] = []
  for (const file of new Bun.Glob("{src,web}/**/*.{ts,tsx,html,css}").scanSync({ cwd: root })) {
    const text = readFileSync(join(root, file), "utf8")
    for (const match of text.matchAll(/(?:from|import|src=|href=)\s*\(?\s*["'](\.{1,2}\/[^"']*)["']/g)) {
      const target = resolve(dirname(join(root, file)), match[1]!)
      if (!target.startsWith(`${resolve(root)}/`)) escaping.push(`${file}: ${match[1]}`)
    }
  }
  expect(escaping).toEqual([])
})
