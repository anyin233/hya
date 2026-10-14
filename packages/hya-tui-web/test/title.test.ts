import { expect, test } from "bun:test"
import { pageTitle } from "../src/title"

test("pageTitle shows the terminal title, or the page's own title when it is cleared", () => {
  expect(pageTitle("● Fix login bug · web", "hya")).toBe("● Fix login bug · web")
  expect(pageTitle("", "hya")).toBe("hya")
  expect(pageTitle("   ", "hya")).toBe("hya")
})
