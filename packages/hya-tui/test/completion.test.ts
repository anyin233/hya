import { expect, test } from "bun:test"
import { completeCommand, SecretEntry } from "../src/completion"

const context = {
  backendCommands: ["compact", "review"],
  providers: ["anthropic", "openai"],
  savedKeys: ["anthropic"],
  models: ["anthropic/claude", "openai/gpt"],
  sessions: ["hysec_1"],
  workflows: ["release"],
  interactions: ["req_1"],
  agents: ["build"],
  apiOperations: ["GET /v1/health", "GET /v1/models"],
}

test("completes native and backend slash commands with current catalog arguments", () => {
  expect(completeCommand("/mo", context)).toEqual(["/model", "/models"])
  expect(completeCommand("/com", context)).toEqual(["/compact"])
  expect(completeCommand("/key s", context)).toEqual(["/key set"])
  expect(completeCommand("/key remove a", context)).toEqual(["/key remove anthropic"])
  expect(completeCommand("/model anth", context)).toEqual(["/model anthropic/claude"])
  expect(completeCommand("/workflow run r", context)).toEqual(["/workflow run release"])
  expect(completeCommand("/api GET /v1/m", context)).toEqual(["/api GET /v1/models"])
  expect(completeCommand("ordinary prompt", context)).toEqual([])
})

test("secret entry renders only bullets and clears the key after submission", () => {
  const entry = new SecretEntry()
  entry.append("sk-secret\n")
  expect(entry.mask).toBe("•••••••••")
  entry.backspace()
  expect(entry.take()).toBe("sk-secre")
  expect(entry.mask).toBe("")
})
