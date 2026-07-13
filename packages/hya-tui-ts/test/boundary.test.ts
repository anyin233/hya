import { expect, test } from "bun:test"
import { readdir, readFile } from "node:fs/promises"
import { isBuiltin } from "node:module"
import path from "node:path"

const root = path.resolve(import.meta.dir, "..")
const upstreamLicense = `MIT License

Copyright (c) 2025 opencode

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
`
const dependencyPins = {
  "@opencode-ai/plugin": "1.17.9",
  "@opencode-ai/sdk": "1.17.9",
  "@opentui/core": "0.3.4",
  "@opentui/keymap": "0.3.4",
  "@opentui/solid": "0.3.4",
  clipboardy: "4.0.0",
  diff: "8.0.2",
  effect: "4.0.0-beta.83",
  fuzzysort: "3.1.0",
  open: "10.1.2",
  "opentui-spinner": "0.0.7",
  remeda: "2.26.0",
  "solid-js": "1.9.10",
  "strip-ansi": "7.1.2",
}
const devDependencyPins = {
  "@tsconfig/bun": "1.0.9",
  "@types/bun": "1.3.13",
  "@typescript/native-preview": "7.0.0-dev.20251207.1",
}
const sourcePrefixes = [
  "hya/",
  "upstream/assets/",
  "upstream/component/",
  "upstream/config/",
  "upstream/context/",
  "upstream/feature-plugins/",
  "upstream/plugin/",
  "upstream/prompt/",
  "upstream/routes/",
  "upstream/theme/",
  "upstream/ui/",
  "upstream/util/",
]
const sourceFiles = new Set([
  "main.tsx",
  "upstream/app.tsx",
  "upstream/attention.ts",
  "upstream/audio.d.ts",
  "upstream/audio.ts",
  "upstream/clipboard.ts",
  "upstream/editor-zed.ts",
  "upstream/editor.ts",
  "upstream/index.tsx",
  "upstream/keymap.tsx",
  "upstream/logo.ts",
  "upstream/parsers-config.ts",
  "upstream/runtime.tsx",
  "upstream/terminal-win32.ts",
])
const forbiddenPath =
  /(^|\/)(backend|server|provider|worker|updater|console)(\/|$)|dialog-console-org|system\/plugins/i
const forbiddenImport =
  /@opencode-ai\/(core|ui|provider)(\/|$)|(^|\/)(backend|server|provider|worker|updater|console)(\/|$)/i

async function filesUnder(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true })
  const files = await Promise.all(
    entries.map(async (entry) => {
      const absolute = path.join(directory, entry.name)
      return entry.isDirectory() ? filesUnder(absolute) : [absolute]
    }),
  )
  return files.flat()
}

test("retained frontend stays inside the pinned legal and source boundary", async () => {
  expect(await readFile(path.join(root, "LICENSE"), "utf8")).toBe(upstreamLicense)

  const provenance = await readFile(path.join(root, "UPSTREAM.md"), "utf8")
  for (const required of [
    "https://github.com/anomalyco/opencode",
    "1.17.9",
    "cf31029350820c6bfc0fbd0e052a79a067ee6116",
    "packages/tui",
    "Imported boundary",
    "Excluded boundary",
  ]) {
    expect(provenance).toContain(required)
  }

  const manifest = await Bun.file(path.join(root, "package.json")).json()
  expect(manifest.private).toBe(true)
  expect(manifest.packageManager).toBe("bun@1.3.14")
  expect(manifest.dependencies).toEqual(dependencyPins)
  expect(manifest.devDependencies).toEqual(devDependencyPins)

  const sourceDirectory = path.join(root, "src")
  const sources = await filesUnder(sourceDirectory)
  expect(sources.length).toBeGreaterThan(0)

  for (const absolute of sources) {
    const relative = path.relative(sourceDirectory, absolute).split(path.sep).join("/")
    expect(sourceFiles.has(relative) || sourcePrefixes.some((prefix) => relative.startsWith(prefix))).toBe(true)
    expect(relative).not.toMatch(forbiddenPath)
    expect(relative).toMatch(/\.(ts|tsx|json|mp3)$/)

    if (!/\.(ts|tsx)$/.test(relative)) continue
    const source = await readFile(absolute, "utf8")
    const imports = [
      ...source.matchAll(/^\s*(?:import|export)\s+(?:[^"']*?\sfrom\s+)?(["'])([^"']+)\1/gm),
      ...source.matchAll(/import\(\s*(["'])([^"']+)\1\s*\)/g),
    ]
    for (const match of imports) {
      const specifier = match[2] ?? ""
      expect(specifier).not.toMatch(forbiddenImport)
      if (specifier.startsWith(".") || specifier.startsWith("bun:") || isBuiltin(specifier)) continue
      const dependency = specifier.startsWith("@") ? specifier.split("/").slice(0, 2).join("/") : specifier.split("/")[0]
      expect(dependencyPins).toHaveProperty(dependency)
    }
  }
})
