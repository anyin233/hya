// A direct gRPC frontend uses the same Project, session, and live turn flow as HTTP.
import { writeFile } from "node:fs/promises"
import { join } from "node:path"
import { expect, expectStatus, test, textStep, tuiMain } from "./hya"

test.use({ model: { steps: [textStep("reply over grpc")] } })

test("--grpc opens a session and renders a live reply", async ({ backend, tui }, testInfo) => {
  await writeFile(join(backend.dir, "workspace-file.txt"), "gRPC lookup probe\n")
  const listener = new URL(backend.url).host
  const term = await tui(["bun", tuiMain, "--grpc", listener, "--dir", backend.dir])
  await term.waitForText("Message, !shell, or @file · / commands")
  await expectStatus(term, "Server", `grpc://${listener}`)

  await term.type("hello through grpc @workspace")
  await term.waitForText("workspace-file.txt")
  await term.press("Tab")
  await term.press("Enter")
  await term.waitForText("reply over grpc", 20_000)
  expect(await term.find("hello through grpc")).not.toBeNull()
  await term.attach(testInfo, "grpc-conversation")
})
