import fs from "node:fs"
import path from "node:path"

import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makeTempDir } from "./helpers"
import {
  MarketplaceError,
  readMarketplace,
  resolveMarketplaceEntry,
  withResolvedMarketplaceEntry,
} from "../src/marketplace"

afterEach(cleanupTempDirs)

function writeMarketplace(root: string, document: unknown): void {
  fs.mkdirSync(root, { recursive: true })
  fs.writeFileSync(path.join(root, "marketplace.json"), JSON.stringify(document))
}

describe("readMarketplace", () => {
  test("parses local and unsupported entries", () => {
    const root = "/tmp/not-real-market"
    writeMarketplace(root, {
      name: "demo",
      plugins: [
        { name: "local-one", source: "./plugins/local-one" },
        { name: "default-dir" },
        { name: "git-one", source: { source: "git", repo: "https://example.com/repo" } },
        { name: "" },
      ],
    })
    const marketplace = readMarketplace(root)
    expect(marketplace.name).toBe("demo")
    expect(marketplace.plugins).toEqual([
      { name: "local-one", localPath: "./plugins/local-one" },
      { name: "default-dir", localPath: "./default-dir" },
      {
        name: "git-one",
        gitUrl: "https://example.com/repo",
      },
    ])
  })

  test("throws on missing and malformed manifests", async () => {
    const empty = await makeTempDir()
    expect(() => readMarketplace(empty)).toThrow(MarketplaceError)
    const broken = await makeTempDir()
    fs.writeFileSync(path.join(broken, "marketplace.json"), "{oops")
    expect(() => readMarketplace(broken)).toThrow(MarketplaceError)
  })
})

describe("withResolvedMarketplaceEntry", () => {
  test("shallow-clones a git source for the action and removes it afterward", async () => {
    const repo = await makeTempDir("hya-claude-git-")
    fs.writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ name: "cloned" }))
    for (const args of [["init"], ["add", "plugin.json"], ["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-m", "fixture"]]) {
      const run = Bun.spawnSync(["git", ...args], { cwd: repo })
      expect(run.exitCode).toBe(0)
    }
    let checkout = ""
    const name = await withResolvedMarketplaceEntry(
      { root: repo, name: "fixture", plugins: [] },
      { name: "cloned", gitUrl: `file://${repo}` },
      async (pluginDir) => {
        checkout = pluginDir
        return JSON.parse(fs.readFileSync(path.join(pluginDir, "plugin.json"), "utf8")).name as string
      },
    )
    expect(name).toBe("cloned")
    expect(fs.existsSync(checkout)).toBe(false)
  })
})

describe("resolveMarketplaceEntry", () => {
  test("resolves local entries inside the root", () => {
    const marketplace = {
      root: "/tmp/market",
      name: "market",
      plugins: [],
    }
    expect(
      resolveMarketplaceEntry(marketplace, { name: "a", localPath: "./plugins/a" }),
    ).toBe("/tmp/market/plugins/a")
  })

  test("rejects traversal and unsupported entries", () => {
    const marketplace = { root: "/tmp/market", name: "market", plugins: [] }
    expect(() =>
      resolveMarketplaceEntry(marketplace, { name: "bad", localPath: "../outside" }),
    ).toThrow(MarketplaceError)
    expect(() =>
      resolveMarketplaceEntry(marketplace, {
        name: "git",
        unsupportedReason: "git is not supported",
      }),
    ).toThrow("git is not supported")
  })
})
