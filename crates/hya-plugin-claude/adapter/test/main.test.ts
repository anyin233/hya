import { afterEach, describe, expect, test } from "bun:test"

import { cleanupTempDirs, makePluginDir, runCli } from "./helpers"

afterEach(cleanupTempDirs)

describe("CLI argument handling", () => {
  test("--help and --version exit cleanly", async () => {
    const help = await runCli(["--help"])
    expect(help.exitCode).toBe(0)
    expect(help.stdout).toContain("hya-claude-adapter")
    const version = await runCli(["--version"])
    expect(version.exitCode).toBe(0)
    expect(version.stdout.trim()).toBe("1.0.0")
  })

  test("missing --plugin-dir fails with usage guidance", async () => {
    const missing = await runCli([])
    expect(missing.exitCode).toBe(1)
    expect(missing.stderr).toContain("--plugin-dir")
    const unknown = await runCli(["--wat"])
    expect(unknown.exitCode).toBe(1)
    const dangling = await runCli(["--plugin-dir"])
    expect(dangling.exitCode).toBe(1)
  })

  test("emit mode requires --plugin-dir too", async () => {
    const missing = await runCli(["--emit-bundle-manifest"])
    expect(missing.exitCode).toBe(1)
  })

  test("relative plugin dirs resolve against cwd", async () => {
    const dir = await makePluginDir({ name: "relative" })
    const emit = await runCli(["--emit-bundle-manifest", "--plugin-dir", dir])
    expect(emit.exitCode).toBe(0)
  })
})
