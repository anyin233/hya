import { expect, test } from "bun:test"
import { atBottom, pageStep, ScrollFollow } from "../src/state/scroll"

const at = (scrollTop: number, scrollHeight: number, viewportHeight = 10) => ({ scrollTop, scrollHeight, viewportHeight })

test("at the bottom means the last row is in view (one row of slack)", () => {
  expect(atBottom(at(0, 5))).toBe(true)
  expect(atBottom(at(10, 20))).toBe(true)
  expect(atBottom(at(9, 20))).toBe(true)
  expect(atBottom(at(8, 20))).toBe(false)
})

test("a page is the viewport minus two rows of overlap", () => {
  expect(pageStep(20)).toBe(18)
  expect(pageStep(2)).toBe(1)
})

test("content growing while following the bottom never raises the hint", () => {
  const follow = new ScrollFollow()
  expect(follow.observe(at(10, 20))).toEqual({ atBottom: true, unseen: false })
  // Grew before the sticky scroll caught up: still following.
  expect(follow.observe(at(10, 30))).toEqual({ atBottom: false, unseen: false })
  expect(follow.observe(at(20, 30))).toEqual({ atBottom: true, unseen: false })
})

test("new content below a scrolled-up view raises the hint until the bottom is reached", () => {
  const follow = new ScrollFollow()
  follow.observe(at(20, 30))
  // The user scrolls up: no new content yet, no hint.
  expect(follow.observe(at(5, 30))).toEqual({ atBottom: false, unseen: false })
  expect(follow.observe(at(5, 34))).toEqual({ atBottom: false, unseen: true })
  // Stays up while more arrives or the user scrolls, but not yet at the bottom.
  expect(follow.observe(at(12, 40))).toEqual({ atBottom: false, unseen: true })
  expect(follow.observe(at(30, 40))).toEqual({ atBottom: true, unseen: false })
})

test("reset forgets the previous transcript (session switch)", () => {
  const follow = new ScrollFollow()
  follow.observe(at(20, 30))
  follow.observe(at(0, 30))
  follow.observe(at(0, 50))
  follow.reset()
  expect(follow.observe(at(0, 50))).toEqual({ atBottom: false, unseen: false })
})
