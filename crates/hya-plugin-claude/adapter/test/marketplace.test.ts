import fs from "node:fs"
import path from "node:path"

import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makeTempDir } from "./helpers"
import {
  MarketplaceError,
  readMarketplace,
  resolveMarketplaceEntry,
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
        { name: "git-one", source: "git", repo: "https://example.com/repo" },
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
        unsupportedReason:
          'source "git" is not a local path (v1 installs local plugin directories only)',
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
