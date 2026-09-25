import { expect, test } from "bun:test"
import { parseArguments, usage } from "../src/cli"

test("parses --server and --dir and rejects unknown flags", () => {
  expect(parseArguments(["--server", "http://localhost:9000", "--dir", "/tmp"], "/cwd")).toEqual({
    server: "http://localhost:9000/", directory: "/tmp", continue: false,
  })
  expect(parseArguments(["--help"], "/cwd")).toBeNull()
  expect(parseArguments(["-h"], "/cwd")).toBeNull()
  expect(() => parseArguments(["--bogus"], "/cwd")).toThrow("Unknown or incomplete option: --bogus")
  expect(() => parseArguments(["--server", "ftp://x"], "/cwd")).toThrow("--server needs an HTTP URL")
})

test("without --server the TUI starts its own backend (no server URL is set)", () => {
  expect(parseArguments([], "/cwd")).toEqual({ directory: "/cwd", continue: false })
  expect(parseArguments(["--hya", "/opt/hya", "--db", "/tmp/s.db"], "/cwd")).toEqual({
    directory: "/cwd", continue: false, hya: "/opt/hya", db: "/tmp/s.db",
  })
})

test("--continue and --session choose the session to open", () => {
  expect(parseArguments(["--continue"], "/cwd")).toMatchObject({ continue: true })
  expect(parseArguments(["-c"], "/cwd")).toMatchObject({ continue: true })
  expect(parseArguments(["--session", "hysec_1"], "/cwd")).toMatchObject({ session: "hysec_1", continue: false })
  expect(parseArguments(["-s", "hysec_1"], "/cwd")).toMatchObject({ session: "hysec_1" })
  expect(() => parseArguments(["--session"], "/cwd")).toThrow("Unknown or incomplete option: --session")
  expect(() => parseArguments(["--continue", "--session", "x"], "/cwd")).toThrow("--continue and --session cannot be combined")
})

test("--hya and --db only apply to a backend the TUI starts itself", () => {
  expect(() => parseArguments(["--server", "http://127.0.0.1:1", "--hya", "/x"], "/cwd")).toThrow("--hya and --db only apply without --server")
  expect(() => parseArguments(["--server", "http://127.0.0.1:1", "--db", "/x"], "/cwd")).toThrow("--hya and --db only apply without --server")
})

test("usage documents every flag and the binary lookup order", () => {
  for (const flag of ["--server", "--dir", "--hya", "--db", "--continue", "--session", "--help"]) expect(usage).toContain(flag)
  expect(usage).toContain("HYA_BIN")
  expect(usage.indexOf("--hya")).toBeLessThan(usage.indexOf("HYA_BIN"))
  expect(usage.indexOf("HYA_BIN")).toBeLessThan(usage.indexOf("hya on PATH"))
})
