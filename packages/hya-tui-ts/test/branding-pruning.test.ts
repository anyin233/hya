import { expect, test } from "bun:test"
import { readdir, readFile } from "node:fs/promises"
import path from "node:path"

const root = path.resolve(import.meta.dir, "..")
const sourceRoot = path.join(root, "src")

async function sourceFiles() {
  const walk = async (directory: string): Promise<string[]> => {
    const entries = await readdir(directory, { withFileTypes: true })
    return (
      await Promise.all(entries.map((entry) => (entry.isDirectory() ? walk(path.join(directory, entry.name)) : path.join(directory, entry.name))))
    )
      .flat()
      .filter((file) => /\.(ts|tsx|json)$/.test(file))
  }
  return walk(sourceRoot)
}

test("public hya presentation and static registrations are stable", async () => {
  const { auditSurface } = await import("../src/hya/audit")

  expect(auditSurface.product).toBe("hya")
  expect(new Set(Object.values(auditSurface.presentation))).toEqual(new Set(["hya"]))
  expect(auditSurface.terminalTitle()).toBe("hya")
  expect(auditSurface.terminalTitle("Session title")).toBe("hya | Session title")
  expect(auditSurface.defaultTheme).toBe("hya")
  expect(auditSurface.defaultSoundPack).toBe("hya.default")
  expect(auditSurface.paths.every((item) => item.split(path.sep).includes("hya"))).toBe(true)
  expect(auditSurface.tempName).toStartWith("hya-")
  expect(auditSurface.staticBuiltins).toEqual([
    "internal:home-footer",
    "internal:home-tips",
    "internal:sidebar-context",
    "internal:sidebar-mcp",
    "internal:sidebar-lsp",
    "internal:sidebar-todo",
    "internal:sidebar-files",
    "internal:sidebar-footer",
    "internal:notifications",
    "which-key",
    "diff-viewer",
  ])
  expect(auditSurface.commands).toContain("hya.status")
  expect(auditSurface.commands).not.toEqual(
    expect.arrayContaining([
      "docs.open",
      "provider.connect",
      "session.share",
      "session.unshare",
      "workspace.list",
      "workspace.set",
      "console.org.switch",
      "plugins.list",
      "plugins.install",
    ]),
  )
})

test("reachable source contains no unsupported controls or product branding", async () => {
  const files = await sourceFiles()
  const forbiddenFeature =
    /global\.upgrade|experimental\.console|console\.org|session\.(?:un)?share|workspace\.(?:list|set|create|remove|warp|adapter)|provider\.auth|plugins\.(?:list|install)|opencode\.ai|opencode\s+mcp\s+auth|consoleManagedProviders|isConsoleManagedProvider|Open console for more details|dialog-provider|DialogWorkspace|DialogRetryAction/i
  // Stored user-state/protocol constants, not displayed branding, so they remain allowed.
  const allowedProtocolStrings = new Set(["opencode.mode", "opencode-plain-text"])

  for (const file of files) {
    const relative = path.relative(sourceRoot, file).split(path.sep).join("/")
    const source = await readFile(file, "utf8")
    expect(source, relative).not.toMatch(forbiddenFeature)
    expect(source, relative).not.toMatch(/from ["'][^"']*(?:dist|build)[^"']*["']/)

    if (file.endsWith(".json")) {
      expect(source, relative).not.toMatch(/OpenCode|opencode|\bOC\b/)
      continue
    }

    const withoutComments = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|\s)\/\/.*$/gm, "$1")
    for (const match of withoutComments.matchAll(/(["'`])([^"'`\n]*(?:OpenCode|opencode|\bOC\b)[^"'`\n]*)\1/g)) {
      const value = match[2] ?? ""
      if (value.startsWith("@opencode-ai/")) continue
      if (allowedProtocolStrings.has(value)) continue
      throw new Error(`${relative}: unexpected OpenCode string ${JSON.stringify(value)}`)
    }
  }
})
