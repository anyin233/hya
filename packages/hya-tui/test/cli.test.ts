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

test("--web-url and --web-error carry the WebUI state from bare hya", () => {
  expect(parseArguments(["--server", "http://127.0.0.1:1", "--web-url", "http://127.0.0.1:3250/"], "/cwd")).toMatchObject({
    server: "http://127.0.0.1:1/", web: { url: "http://127.0.0.1:3250/" },
  })
  expect(parseArguments(["--web-error", "port 3250 is in use"], "/cwd")).toMatchObject({ web: { error: "port 3250 is in use" } })
  expect(parseArguments([], "/cwd")?.web).toBeUndefined()
  expect(() => parseArguments(["--web-url", "http://x/", "--web-error", "y"], "/cwd")).toThrow("--web-url and --web-error cannot be combined")
  expect(() => parseArguments(["--web-url", "ftp://x"], "/cwd")).toThrow("--web-url needs an HTTP URL")
  for (const flag of ["--web-url", "--web-error"]) expect(usage).toContain(flag)
})

test("usage names the preferences file and its HYA_TUI_CONFIG override", () => {
  expect(usage).toContain("HYA_TUI_CONFIG")
  expect(usage).toContain("hya/tui.json")
})

test("--attached-pid names the running server bare hya attached to", () => {
  expect(parseArguments(["--server", "http://127.0.0.1:1", "--attached-pid", "4242"], "/cwd")).toMatchObject({ attachedPid: 4242 })
  expect(() => parseArguments(["--attached-pid", "4242"], "/cwd")).toThrow("--attached-pid only applies with --server")
  expect(() => parseArguments(["--server", "http://127.0.0.1:1", "--attached-pid", "x"], "/cwd")).toThrow("--attached-pid needs a process id")
  expect(usage).toContain("--attached-pid")
})
