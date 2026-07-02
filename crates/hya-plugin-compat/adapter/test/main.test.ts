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
  expect(stdout.trim()).toBe("0.0.0")
})
