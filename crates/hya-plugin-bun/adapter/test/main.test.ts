import { expect, test } from "bun:test"

test("version command exits successfully", async () => {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts", "--version"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    stdout: "pipe",
    stderr: "pipe",
  })

  const [stdout, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    proc.exited,
  ])

  expect(exitCode).toBe(0)
  expect(stdout.trim()).toBe("1.0.0")
})

test("help command prints usage", async () => {
  const proc = Bun.spawn([process.execPath, "run", "src/main.ts", "--help"], {
    cwd: import.meta.dir.replace(/\/test$/, ""),
    stdout: "pipe",
    stderr: "pipe",
  })

  const [stdout, exitCode] = await Promise.all([
    new Response(proc.stdout).text(),
    proc.exited,
  ])

  expect(exitCode).toBe(0)
  expect(stdout).toContain("--bundle-extension")
  expect(stdout).toContain("--extension")
})

test("rejects relative extension paths", async () => {
  const proc = Bun.spawn(
    [process.execPath, "run", "src/main.ts", "--bundle-extension", "relative.js"],
    {
      cwd: import.meta.dir.replace(/\/test$/, ""),
      stdout: "pipe",
      stderr: "pipe",
    },
  )

  const [stderr, exitCode] = await Promise.all([
    new Response(proc.stderr).text(),
    proc.exited,
  ])

  expect(exitCode).toBe(1)
  expect(stderr).toContain("requires an absolute path")
})
