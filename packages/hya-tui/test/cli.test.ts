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

test("with --server, --db and --hya let the TUI find or restart the database's daemon when that server goes away", () => {
  expect(parseArguments(["--server", "http://127.0.0.1:1", "--db", "/s.db", "--hya", "/bin/hya"], "/cwd")).toEqual({
    server: "http://127.0.0.1:1/", directory: "/cwd", continue: false, db: "/s.db", hya: "/bin/hya",
  })
  expect(usage).toMatch(/--db PATH[\s\S]*--server/)
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

test("--attached-pid is gone: the backend is a daemon nobody owns", () => {
  expect(() => parseArguments(["--server", "http://127.0.0.1:1", "--attached-pid", "4242"], "/cwd")).toThrow("Unknown or incomplete option: --attached-pid")
  expect(usage).not.toContain("--attached-pid")
})

test("--resume [id] reopens a session (and clears its archived state); without an id it asks with a picker", () => {
  expect(parseArguments(["--resume"], "/cwd")).toEqual({ directory: "/cwd", continue: false, resume: {} })
  expect(parseArguments(["--resume", "hysec_1"], "/cwd")).toEqual({ directory: "/cwd", continue: false, resume: { id: "hysec_1" } })
  // A flag after it is not an id.
  expect(parseArguments(["--resume", "--dir", "/w"], "/cwd")).toEqual({ directory: "/w", continue: false, resume: {} })
  expect(() => parseArguments(["--resume", "--continue"], "/cwd")).toThrow("--resume cannot be combined with --continue or --session")
  expect(() => parseArguments(["--session", "a", "--resume", "b"], "/cwd")).toThrow("--resume cannot be combined with --continue or --session")
  expect(usage).toContain("--resume [ID]")
})

test("--web-tab marks a TUI that runs in a WebUI tab (bare hya's host command sets it)", () => {
  expect(parseArguments(["--web-tab"], "/cwd")).toEqual({ directory: "/cwd", continue: false, webTab: true })
  expect(parseArguments([], "/cwd")?.webTab).toBeUndefined()
  expect(usage).toContain("--web-tab")
})
