import { expect, test } from "bun:test"
import { parseArguments, usage } from "../src/cli"

test("parses --server and --dir and rejects unknown flags", () => {
  expect(parseArguments(["--server", "http://localhost:9000", "--dir", "/tmp"], "/cwd")).toEqual({
    server: "http://localhost:9000/", directory: "/tmp",
  })
  expect(parseArguments([], "/cwd")).toEqual({ server: "http://127.0.0.1:8080/", directory: "/cwd" })
  expect(parseArguments(["--help"], "/cwd")).toBeNull()
  expect(parseArguments(["-h"], "/cwd")).toBeNull()
  expect(() => parseArguments(["--bogus"], "/cwd")).toThrow("Unknown or incomplete option: --bogus")
  expect(() => parseArguments(["--server", "ftp://x"], "/cwd")).toThrow("--server needs an HTTP URL")
  expect(usage).toBe("Usage: bun packages/hya-tui/src/main.ts [--server http://127.0.0.1:8080] [--dir PATH]\n")
})
