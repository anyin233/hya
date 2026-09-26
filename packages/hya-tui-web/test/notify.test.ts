import { describe, expect, test } from "bun:test"
import { createNotificationDeduper, notificationTag, osc777Notification, osc9Notification, shouldShowNotification } from "../src/notify"

describe("OSC 9 / OSC 777 desktop notifications", () => {
  test("OSC 9's payload is the body, with no title", () => {
    expect(osc9Notification("Turn finished · demo")).toEqual({ title: "", body: "Turn finished · demo" })
    expect(osc9Notification("")).toEqual({ title: "", body: "" })
  })

  test("OSC 777 notify splits into title and body", () => {
    expect(osc777Notification("notify;hya;Turn finished · demo")).toEqual({ title: "hya", body: "Turn finished · demo" })
  })

  test("a body containing ';' is kept whole", () => {
    expect(osc777Notification("notify;hya;a;b;c")).toEqual({ title: "hya", body: "a;b;c" })
  })

  test("a non-notify subcommand, or one missing its separators, is ignored", () => {
    expect(osc777Notification("other;x;y")).toBeUndefined()
    expect(osc777Notification("notify")).toBeUndefined()
    expect(osc777Notification("")).toBeUndefined()
  })

  test("shouldShowNotification: hidden or unfocused, not both required", () => {
    expect(shouldShowNotification({ hidden: true, focused: false })).toBe(true)
    expect(shouldShowNotification({ hidden: true, focused: true })).toBe(true)
    expect(shouldShowNotification({ hidden: false, focused: false })).toBe(true)
    expect(shouldShowNotification({ hidden: false, focused: true })).toBe(false)
  })

  test("notificationTag combines title and body, so a plain OSC 9 (no title) does not collide with an unrelated OSC 777 title with the same body", () => {
    expect(notificationTag({ title: "hya", body: "Turn finished" })).toBe(notificationTag({ title: "hya", body: "Turn finished" }))
    expect(notificationTag({ title: "", body: "Turn finished" })).not.toBe(notificationTag({ title: "hya", body: "Turn finished" }))
  })

  test("createNotificationDeduper suppresses a same-body repeat within the window", () => {
    const deduper = createNotificationDeduper(250)
    expect(deduper.shouldShow("Turn finished", 1000)).toBe(true)
    // OSC 777 arriving right after OSC 9 with the same body: suppressed.
    expect(deduper.shouldShow("Turn finished", 1010)).toBe(false)
    // A different body is never suppressed.
    expect(deduper.shouldShow("Turn failed", 1010)).toBe(true)
  })

  test("createNotificationDeduper allows the same body again once the window has passed", () => {
    const deduper = createNotificationDeduper(250)
    expect(deduper.shouldShow("Turn finished", 1000)).toBe(true)
    expect(deduper.shouldShow("Turn finished", 1200)).toBe(false)
    expect(deduper.shouldShow("Turn finished", 1300)).toBe(true)
  })
})
