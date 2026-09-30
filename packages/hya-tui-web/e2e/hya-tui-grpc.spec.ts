// A direct gRPC frontend uses the same Project, session, and live turn flow as HTTP.
import { expect, test, textStep, tuiMain } from "./hya"

test.use({ model: { steps: [textStep("reply over grpc")] } })

test("--grpc opens a session and renders a live reply", async ({ backend, tui }, testInfo) => {
  const listener = new URL(backend.url).host
  const term = await tui(["bun", tuiMain, "--grpc", listener, "--dir", backend.dir])
  await term.waitForText("Message, !shell, or @file · / commands")
  await term.type("/status")
  await term.press("Enter")
  await term.waitForText(`grpc://${listener}`)

  await term.type("hello through grpc")
  await term.press("Enter")
  await term.waitForText("reply over grpc", 20_000)
  expect(await term.find("hello through grpc")).not.toBeNull()
  await term.attach(testInfo, "grpc-conversation")
})
