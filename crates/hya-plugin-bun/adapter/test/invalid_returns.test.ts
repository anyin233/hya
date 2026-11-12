import { expect, spyOn, test } from "bun:test"
import { runChatParamsHooks } from "../src/chat_params_hooks"
import { runToolExecuteAfterHooks } from "../src/hooks"
import { loadExtensionContributions } from "../src/loader/init"

test("invalid replacement returns identify the hook and extension on stderr", async () => {
  const spec = new URL("./fixtures/invalid-returns.ts", import.meta.url).pathname
  const loaded = await loadExtensionContributions([spec], {})
  expect(loaded.errors).toEqual([])
  const fixtures = await Bun.file(new URL("../../../hya-plugin/tests/fixtures/hook-wire.json", import.meta.url)).json()
  const stderr = spyOn(console, "error").mockImplementation(() => {})
  try {
    await runChatParamsHooks(loaded.hooks, fixtures["chat.params"].params)
    await runToolExecuteAfterHooks(loaded.hooks, fixtures["tool.execute.after"].params)
    expect(stderr).toHaveBeenCalledTimes(2)
    expect(stderr.mock.calls[0]?.[0]).toContain("chat.params")
    expect(stderr.mock.calls[1]?.[0]).toContain("tool.execute.after")
    for (const call of stderr.mock.calls) expect(call[0]).toContain(spec)
  } finally {
    stderr.mockRestore()
  }
})
