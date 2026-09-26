import { expect, test } from "bun:test"
import { notificationBody, notificationSequence, sanitizeNotificationText, shouldNotify } from "../src/notify"

test("shouldNotify: on and unfocused only", () => {
  expect(shouldNotify({ notifications: true, focused: false })).toBe(true)
  expect(shouldNotify({ notifications: true, focused: true })).toBe(false)
  expect(shouldNotify({ notifications: false, focused: false })).toBe(false)
  expect(shouldNotify({ notifications: false, focused: true })).toBe(false)
})

test("sanitizeNotificationText strips control characters and collapses whitespace", () => {
  expect(sanitizeNotificationText("hi\x07there\x1bnow")).toBe("hi there now")
  expect(sanitizeNotificationText("  a   b  ")).toBe("a b")
})

test("sanitizeNotificationText truncates with an ellipsis", () => {
  const long = "x".repeat(200)
  const result = sanitizeNotificationText(long, 10)
  expect(result.length).toBe(10)
  expect(result.endsWith("…")).toBe(true)
})

test("notificationBody builds the message per kind", () => {
  expect(notificationBody("turnFinished", "my session")).toBe("Turn finished · my session")
  expect(notificationBody("turnFinished", "")).toBe("Turn finished")
  expect(notificationBody("turnFailed", "boom")).toBe("Turn failed: boom")
  expect(notificationBody("permission", "bash")).toBe("Permission needed: bash")
  expect(notificationBody("question", "Which one?")).toBe("Question: Which one?")
})

test("notificationSequence emits OSC 9 and OSC 777 with the sanitized body and title", () => {
  const sequence = notificationSequence("Turn finished · demo")
  expect(sequence).toBe("\x1b]9;Turn finished · demo\x07\x1b]777;notify;hya;Turn finished · demo\x07")
})

test("notificationSequence sanitizes control characters out of the body and title", () => {
  const sequence = notificationSequence("bad\x07body", "bad\x1btitle")
  expect(sequence).toBe("\x1b]9;bad body\x07\x1b]777;notify;bad title;bad body\x07")
})
